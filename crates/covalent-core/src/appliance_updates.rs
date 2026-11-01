//! A phone's explicit choice for the Pi's automatic-update policy.
//!
//! Verification requires the privileged caller's independently approved bond
//! and current durable settings revision. It does not apply a setting. The
//! caller must verify and atomically publish a new revision under the same lock,
//! including for a no-op choice. Never take the expected revision from a request.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::Signature;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{DeviceIdentity, appliance_import::ImportApprovalBond, identity::signing_message};

pub const AUTOMATIC_UPDATES_DOMAIN: &[u8] = b"covalent/appliance-updates/automatic/v1";
pub const MAX_AUTOMATIC_UPDATE_CHANGE_BYTES: usize = 1280;

/// Field declaration order is the version-1 canonical JSON signing order.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AutomaticUpdateChange {
    pub schema_version: u16,
    pub bond: ImportApprovalBond,
    /// Stable identity of this exact user choice, retained across lost replies.
    pub action_id: Uuid,
    /// Issued and persisted by the Pi, never an address or caller-created nonce.
    pub expected_revision: Uuid,
    pub enabled: bool,
}

/// Only a boolean policy choice is allowed; no token, path, version or URL.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SignedAutomaticUpdateChange {
    pub payload: String,
    pub signature: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("Automatic-update choice rejected")]
pub struct AutomaticUpdateChangeRejected;

type Result<T> = std::result::Result<T, AutomaticUpdateChangeRejected>;

impl AutomaticUpdateChange {
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        self.bond
            .validate()
            .map_err(|_| AutomaticUpdateChangeRejected)?;
        if self.schema_version != 1 || self.action_id.is_nil() || self.expected_revision.is_nil() {
            return Err(AutomaticUpdateChangeRejected);
        }
        let bytes = serde_json::to_vec(self).map_err(|_| AutomaticUpdateChangeRejected)?;
        if bytes.len() > MAX_AUTOMATIC_UPDATE_CHANGE_BYTES {
            return Err(AutomaticUpdateChangeRejected);
        }
        Ok(bytes)
    }
}

pub fn sign_automatic_update_change(
    identity: &DeviceIdentity,
    choice: &AutomaticUpdateChange,
) -> Result<SignedAutomaticUpdateChange> {
    if identity.public_identity() != choice.bond.controller {
        return Err(AutomaticUpdateChangeRejected);
    }
    let bytes = choice.canonical_bytes()?;
    Ok(SignedAutomaticUpdateChange {
        signature: identity.sign(AUTOMATIC_UPDATES_DOMAIN, &bytes),
        payload: String::from_utf8(bytes).map_err(|_| AutomaticUpdateChangeRejected)?,
    })
}

/// Expected values come from locked privileged state, not from `signed.payload`.
/// A read-only duplicate lookup may use a matching persisted receipt's revision.
/// Applying a new choice must instead consume the current revision atomically.
pub fn verify_automatic_update_change(
    expected_bond: &ImportApprovalBond,
    expected_revision: Uuid,
    signed: &SignedAutomaticUpdateChange,
) -> Result<AutomaticUpdateChange> {
    expected_bond
        .validate()
        .map_err(|_| AutomaticUpdateChangeRejected)?;
    if expected_revision.is_nil()
        || signed.payload.len() > MAX_AUTOMATIC_UPDATE_CHANGE_BYTES
        || signed.signature.len() != 86
    {
        return Err(AutomaticUpdateChangeRejected);
    }
    let choice: AutomaticUpdateChange =
        serde_json::from_str(&signed.payload).map_err(|_| AutomaticUpdateChangeRejected)?;
    if choice.bond != *expected_bond
        || choice.expected_revision != expected_revision
        || choice.canonical_bytes()? != signed.payload.as_bytes()
    {
        return Err(AutomaticUpdateChangeRejected);
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(&signed.signature)
        .map_err(|_| AutomaticUpdateChangeRejected)?;
    if bytes.len() != 64 || URL_SAFE_NO_PAD.encode(&bytes) != signed.signature {
        return Err(AutomaticUpdateChangeRejected);
    }
    expected_bond
        .controller
        .verifying_key()
        .map_err(|_| AutomaticUpdateChangeRejected)?
        .verify_strict(
            &signing_message(AUTOMATIC_UPDATES_DOMAIN, signed.payload.as_bytes()),
            &Signature::from_slice(&bytes).map_err(|_| AutomaticUpdateChangeRejected)?,
        )
        .map_err(|_| AutomaticUpdateChangeRejected)?;
    Ok(choice)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appliance_import::{
        IMPORT_APPROVAL_DOMAIN, IMPORT_CAMERA_APPROVAL_DOMAIN, IMPORT_WITHDRAWAL_DOMAIN,
    };
    use serde_json::json;

    fn fixture() -> (DeviceIdentity, AutomaticUpdateChange) {
        let phone = DeviceIdentity::generate();
        let choice = AutomaticUpdateChange {
            schema_version: 1,
            bond: ImportApprovalBond {
                appliance: DeviceIdentity::generate().public_identity(),
                controller: phone.public_identity(),
                root_generation: Uuid::new_v4(),
                native_generation: Uuid::new_v4(),
            },
            action_id: Uuid::new_v4(),
            expected_revision: Uuid::new_v4(),
            enabled: true,
        };
        (phone, choice)
    }

    #[test]
    fn only_the_approved_phone_can_sign_either_explicit_policy_choice() {
        let (phone, mut choice) = fixture();
        for enabled in [true, false] {
            choice.enabled = enabled;
            let proof = sign_automatic_update_change(&phone, &choice).unwrap();
            assert_eq!(
                verify_automatic_update_change(&choice.bond, choice.expected_revision, &proof)
                    .unwrap(),
                choice
            );
            assert_eq!(proof.payload.as_bytes(), choice.canonical_bytes().unwrap());
            assert!(sign_automatic_update_change(&DeviceIdentity::generate(), &choice).is_err());
        }
    }

    #[test]
    fn trusted_revision_and_both_owner_generations_must_still_match() {
        let (phone, choice) = fixture();
        let proof = sign_automatic_update_change(&phone, &choice).unwrap();
        for revision in [Uuid::nil(), Uuid::new_v4()] {
            assert!(verify_automatic_update_change(&choice.bond, revision, &proof).is_err());
        }
        for field in [
            "appliance",
            "controller",
            "rootGeneration",
            "nativeGeneration",
        ] {
            let mut wrong = serde_json::to_value(&choice.bond).unwrap();
            wrong[field] = if matches!(field, "appliance" | "controller") {
                serde_json::to_value(DeviceIdentity::generate().public_identity()).unwrap()
            } else {
                json!(Uuid::new_v4())
            };
            let bond = serde_json::from_value(wrong).unwrap();
            assert!(
                verify_automatic_update_change(&bond, choice.expected_revision, &proof).is_err(),
                "{field}"
            );
        }
    }

    #[test]
    fn changing_any_intent_field_invalidates_the_signature() {
        let (phone, choice) = fixture();
        let proof = sign_automatic_update_change(&phone, &choice).unwrap();
        for field in ["actionId", "expectedRevision", "enabled"] {
            let mut value = serde_json::to_value(&choice).unwrap();
            value[field] = if field == "enabled" {
                json!(false)
            } else {
                json!(Uuid::new_v4())
            };
            let changed: AutomaticUpdateChange = serde_json::from_value(value).unwrap();
            let revision = changed.expected_revision;
            let changed = SignedAutomaticUpdateChange {
                payload: String::from_utf8(changed.canonical_bytes().unwrap()).unwrap(),
                signature: proof.signature.clone(),
            };
            assert!(verify_automatic_update_change(&choice.bond, revision, &changed).is_err());
        }
    }

    #[test]
    fn import_or_withdrawal_signatures_cannot_authorize_update_settings() {
        let (phone, choice) = fixture();
        let mut proof = sign_automatic_update_change(&phone, &choice).unwrap();
        for domain in [
            IMPORT_APPROVAL_DOMAIN,
            IMPORT_CAMERA_APPROVAL_DOMAIN,
            IMPORT_WITHDRAWAL_DOMAIN,
        ] {
            proof.signature = phone.sign(domain, proof.payload.as_bytes());
            assert!(
                verify_automatic_update_change(&choice.bond, choice.expected_revision, &proof)
                    .is_err()
            );
        }
    }

    #[test]
    fn unknown_missing_duplicate_and_noncanonical_fields_are_refused_even_when_signed() {
        let (phone, choice) = fixture();
        let canonical = String::from_utf8(choice.canonical_bytes().unwrap()).unwrap();
        let mut extra = serde_json::to_value(&choice).unwrap();
        extra["token"] = json!("not-allowed");
        let mut wrong_type = serde_json::to_value(&choice).unwrap();
        wrong_type["enabled"] = json!(1);
        let mut missing = serde_json::to_value(&choice).unwrap();
        missing.as_object_mut().unwrap().remove("enabled");
        for payload in [
            format!(" {canonical}"),
            format!("{canonical}\n"),
            serde_json::to_value(&choice).unwrap().to_string(),
            canonical.replacen("\"enabled\":true", "\"enabled\":true,\"enabled\":false", 1),
            extra.to_string(),
            wrong_type.to_string(),
            missing.to_string(),
        ] {
            let proof = SignedAutomaticUpdateChange {
                signature: phone.sign(AUTOMATIC_UPDATES_DOMAIN, payload.as_bytes()),
                payload,
            };
            assert!(
                verify_automatic_update_change(&choice.bond, choice.expected_revision, &proof)
                    .is_err()
            );
        }
    }

    #[test]
    fn malformed_bonds_versions_and_nil_ids_cannot_be_signed() {
        let (phone, original) = fixture();
        for field in [
            "schemaVersion",
            "actionId",
            "expectedRevision",
            "rootGeneration",
            "nativeGeneration",
            "fingerprint",
        ] {
            let mut value = serde_json::to_value(&original).unwrap();
            match field {
                "schemaVersion" => value[field] = json!(2),
                "actionId" | "expectedRevision" => value[field] = json!(Uuid::nil()),
                "fingerprint" => value["bond"]["appliance"][field] = json!("f".repeat(20)),
                _ => value["bond"][field] = json!(Uuid::nil()),
            }
            let choice = serde_json::from_value(value).unwrap();
            assert!(
                sign_automatic_update_change(&phone, &choice).is_err(),
                "{field}"
            );
        }
    }

    #[test]
    fn oversized_payload_and_noncanonical_or_truncated_signatures_are_refused() {
        let (phone, choice) = fixture();
        let proof = sign_automatic_update_change(&phone, &choice).unwrap();
        for signature in [
            proof.signature.clone() + "=",
            proof.signature[..85].to_owned(),
            "a".repeat(86),
            "".into(),
        ] {
            assert!(
                verify_automatic_update_change(
                    &choice.bond,
                    choice.expected_revision,
                    &SignedAutomaticUpdateChange {
                        signature,
                        ..proof.clone()
                    }
                )
                .is_err()
            );
        }
        assert!(
            verify_automatic_update_change(
                &choice.bond,
                choice.expected_revision,
                &SignedAutomaticUpdateChange {
                    payload: " ".repeat(MAX_AUTOMATIC_UPDATE_CHANGE_BYTES + 1),
                    ..proof
                }
            )
            .is_err()
        );
    }
}
