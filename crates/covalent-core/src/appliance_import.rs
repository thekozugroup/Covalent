//! Phone signatures for a fixed import choice, not permission to copy or delete.
//!
//! The privileged caller obtains the expected bond from its own owner-approved
//! private state. After verification it must still admit the exact attachment,
//! preview and durable action tuple under that bond's lock. Never accept the
//! expected bond from the requesting transport or infer it from ordinary pairing.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use covalent_protocol::DeviceId;
use ed25519_dalek::Signature;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{DeviceIdentity, PublicIdentity, identity::signing_message};

/// Separate from pairing, folder-control and ordinary status signatures.
pub const IMPORT_APPROVAL_DOMAIN: &[u8] = b"covalent/appliance-import/request/v1";
/// Withdrawal is a separate change of intent, never inferred from an approval.
pub const IMPORT_WITHDRAWAL_DOMAIN: &[u8] = b"covalent/appliance-import/withdraw/v1";
pub const IMPORT_CAMERA_APPROVAL_DOMAIN: &[u8] = b"covalent/appliance-import/approval/v2";
pub const IMPORT_CAMERA_WITHDRAWAL_DOMAIN: &[u8] = b"covalent/appliance-import/withdraw/v2";
/// Bounds canonical signed text before parsing, signing or verification.
pub const MAX_IMPORT_APPROVAL_BYTES: usize = 2048;

/// Full identities and generations independently approved by the local owner.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportApprovalBond {
    /// The appliance's full pinned identity, never just an address/fingerprint.
    pub appliance: PublicIdentity,
    /// The one explicitly approved phone identity.
    pub controller: PublicIdentity,
    /// Generation of the privileged owner's approval record.
    pub root_generation: Uuid,
    /// Generation of the native node's separate controller bond.
    pub native_generation: Uuid,
}

/// Kind is signed: a camera cannot claim a volume's admission journal.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImportSourceKind {
    /// An observed, pinned removable-volume attachment.
    Volume,
    /// A metadata-preview-bound PTP/MTP camera connection (schema2 only).
    Camera,
}

/// Explicit choices shown to the owner for one attached source.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImportApprovalMode {
    /// Admit this completed preview once.
    Once,
    /// Explicitly register this source for future imports.
    Remember,
    /// Pause this source, including while disconnected.
    Off,
}

/// Import approval never carries source-deletion authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImportRetention {
    /// Keep all originals on their source.
    Retain,
}

/// Fixed declaration order is the version-1 canonical JSON signing order.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportApproval {
    /// Volume schema1 or camera schema2; other combinations fail closed.
    pub schema_version: u16,
    /// Full identities and generations shown for this confirmation.
    pub bond: ImportApprovalBond,
    /// Stable UUID reused only to resolve this same choice after a lost reply.
    pub action_id: Uuid,
    /// Current process session issued by the privileged admission service.
    pub action_session: Uuid,
    /// Positive signed-64-bit durable admission sequence.
    pub action_sequence: u64,
    /// Opaque 64-character lowercase source hash, never a caller-supplied path.
    pub source_id: String,
    /// Selects the source's independent admission service and signing domain.
    pub kind: ImportSourceKind,
    /// Once, explicit remember, or off.
    pub mode: ImportApprovalMode,
    /// Required non-null for once/remember; explicitly null for off.
    pub attachment_id: Option<Uuid>,
    /// Required non-null for once/remember; explicitly null for off.
    pub preview_id: Option<Uuid>,
    /// Retain originals; no cleanup flag exists.
    pub retention: ImportRetention,
}

/// Canonical UTF-8 text and the existing device identity's Ed25519 signature.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SignedImportApproval {
    /// Exact canonical JSON, not a reinterpreted object from the transport.
    pub payload: String,
    /// Canonical unpadded base64url 64-byte signature.
    pub signature: String,
}

/// Fixed error: callers must not expose payloads, keys or parser details.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("Import approval rejected")]
pub struct ImportApprovalRejected;

type Result<T> = std::result::Result<T, ImportApprovalRejected>;

impl ImportApprovalBond {
    /// Validates identities and generations, without granting owner approval.
    pub fn validate(&self) -> Result<()> {
        if self.root_generation.is_nil()
            || self.native_generation.is_nil()
            || self.appliance.device_id == self.controller.device_id
            || self.appliance.public_key == self.controller.public_key
        {
            return Err(ImportApprovalRejected);
        }
        for identity in [&self.appliance, &self.controller] {
            let key = identity
                .verifying_key()
                .map_err(|_| ImportApprovalRejected)?;
            if identity.device_id == DeviceId::from_uuid(Uuid::nil())
                || key.is_weak()
                || URL_SAFE_NO_PAD.encode(key.as_bytes()) != identity.public_key
            {
                return Err(ImportApprovalRejected);
            }
        }
        Ok(())
    }
}

impl ImportApproval {
    pub fn approval_domain(&self) -> &'static [u8] {
        if self.schema_version == 2 {
            IMPORT_CAMERA_APPROVAL_DOMAIN
        } else {
            IMPORT_APPROVAL_DOMAIN
        }
    }
    pub fn withdrawal_domain(&self) -> &'static [u8] {
        if self.schema_version == 2 {
            IMPORT_CAMERA_WITHDRAWAL_DOMAIN
        } else {
            IMPORT_WITHDRAWAL_DOMAIN
        }
    }
    /// Validates the fixed choice before producing its canonical signing bytes.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        self.bond.validate()?;
        if !matches!(
            (self.schema_version, self.kind),
            (1, ImportSourceKind::Volume) | (2, ImportSourceKind::Camera)
        ) || self.action_id.is_nil()
            || self.action_session.is_nil()
            || self.action_sequence == 0
            || self.action_sequence > i64::MAX as u64
            || self.source_id.len() != 64
            || !self
                .source_id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(ImportApprovalRejected);
        }
        let preview = self.attachment_id.is_some_and(|id| !id.is_nil())
            && self.preview_id.is_some_and(|id| !id.is_nil());
        match self.mode {
            ImportApprovalMode::Once | ImportApprovalMode::Remember if preview => {}
            ImportApprovalMode::Off
                if self.attachment_id.is_none() && self.preview_id.is_none() => {}
            _ => return Err(ImportApprovalRejected),
        }
        let bytes = serde_json::to_vec(self).map_err(|_| ImportApprovalRejected)?;
        if bytes.len() > MAX_IMPORT_APPROVAL_BYTES {
            return Err(ImportApprovalRejected);
        }
        Ok(bytes)
    }
}

/// Signs only a valid choice whose controller is this phone's exact identity.
pub fn sign_import_approval(
    phone: &DeviceIdentity,
    choice: &ImportApproval,
) -> Result<SignedImportApproval> {
    if phone.public_identity() != choice.bond.controller {
        return Err(ImportApprovalRejected);
    }
    let bytes = choice.canonical_bytes()?;
    Ok(SignedImportApproval {
        payload: String::from_utf8(bytes.clone()).map_err(|_| ImportApprovalRejected)?,
        signature: phone.sign(choice.approval_domain(), &bytes),
    })
}

/// Verifies against the privileged caller's independent current approved bond.
/// Does not check preview freshness, replay history, copy completion or cloud state.
pub fn verify_import_approval(
    expected: &ImportApprovalBond,
    signed: &SignedImportApproval,
) -> Result<ImportApproval> {
    verify_choice(expected, signed, false)
}

/// Withdraw only this exact approval using a fresh controller signature.
pub fn verify_import_withdrawal(
    expected: &ImportApprovalBond,
    approval: &SignedImportApproval,
    signature: &str,
) -> Result<ImportApproval> {
    verify_import_approval(expected, approval)?;
    verify_choice(
        expected,
        &SignedImportApproval {
            payload: approval.payload.clone(),
            signature: signature.to_owned(),
        },
        true,
    )
}

fn verify_choice(
    expected: &ImportApprovalBond,
    signed: &SignedImportApproval,
    withdrawal: bool,
) -> Result<ImportApproval> {
    expected.validate()?;
    if signed.payload.len() > MAX_IMPORT_APPROVAL_BYTES || signed.signature.len() != 86 {
        return Err(ImportApprovalRejected);
    }
    let choice: ImportApproval =
        serde_json::from_str(&signed.payload).map_err(|_| ImportApprovalRejected)?;
    if choice.bond != *expected || choice.canonical_bytes()? != signed.payload.as_bytes() {
        return Err(ImportApprovalRejected);
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(&signed.signature)
        .map_err(|_| ImportApprovalRejected)?;
    if bytes.len() != 64 || URL_SAFE_NO_PAD.encode(&bytes) != signed.signature {
        return Err(ImportApprovalRejected);
    }
    let signature = Signature::from_slice(&bytes).map_err(|_| ImportApprovalRejected)?;
    expected
        .controller
        .verifying_key()
        .map_err(|_| ImportApprovalRejected)?
        .verify_strict(
            &signing_message(
                if withdrawal {
                    choice.withdrawal_domain()
                } else {
                    choice.approval_domain()
                },
                signed.payload.as_bytes(),
            ),
            &signature,
        )
        .map_err(|_| ImportApprovalRejected)?;
    Ok(choice)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recorded_android_volume_and_camera_proofs_keep_their_original_domains() {
        for raw in [
            include_str!("../../../apps/android/app/src/test/resources/appliance-imports.json"),
            include_str!(
                "../../../apps/android/app/src/test/resources/appliance-camera-imports.json"
            ),
        ] {
            let value: serde_json::Value = serde_json::from_str(raw).unwrap();
            let bond: ImportApprovalBond =
                serde_json::from_value(value["context"]["bond"].clone()).unwrap();
            let proof: SignedImportApproval =
                serde_json::from_value(value["action"]["proof"].clone()).unwrap();
            let choice = verify_import_approval(&bond, &proof).unwrap();
            assert_eq!(choice.canonical_bytes().unwrap(), proof.payload.as_bytes());
            assert!(
                verify_import_withdrawal(
                    &bond,
                    &proof,
                    value["withdrawalSignature"].as_str().unwrap()
                )
                .is_ok()
            );
        }
    }
    #[test]
    fn camera_schema2_signatures_and_withdrawal_cannot_cross_version_kind_or_intent() {
        let (phone, mut camera) = fixture();
        camera.schema_version = 2;
        camera.kind = ImportSourceKind::Camera;
        let proof = sign_import_approval(&phone, &camera).unwrap();
        assert_eq!(
            verify_import_approval(&camera.bond, &proof).unwrap(),
            camera
        );
        let withdrawal = phone.sign(camera.withdrawal_domain(), proof.payload.as_bytes());
        assert!(verify_import_withdrawal(&camera.bond, &proof, &withdrawal).is_ok());
        for domain in [
            IMPORT_APPROVAL_DOMAIN,
            IMPORT_WITHDRAWAL_DOMAIN,
            IMPORT_CAMERA_WITHDRAWAL_DOMAIN,
        ] {
            let wrong = SignedImportApproval {
                payload: proof.payload.clone(),
                signature: phone.sign(domain, proof.payload.as_bytes()),
            };
            assert!(verify_import_approval(&camera.bond, &wrong).is_err());
        }
        assert!(verify_import_withdrawal(&camera.bond, &proof, &proof.signature).is_err());
        for (version, kind) in [
            (1, ImportSourceKind::Camera),
            (2, ImportSourceKind::Volume),
            (3, ImportSourceKind::Camera),
        ] {
            let mut invalid = camera.clone();
            invalid.schema_version = version;
            invalid.kind = kind;
            assert!(invalid.canonical_bytes().is_err());
        }
        let altered = SignedImportApproval {
            payload: proof.payload.replace("\"camera\"", "\"volume\""),
            signature: proof.signature.clone(),
        };
        assert!(verify_import_approval(&camera.bond, &altered).is_err());
    }
    #[test]
    fn withdrawal_is_a_separate_exact_change_of_intent() {
        let (phone, choice) = fixture();
        let approval = sign_import_approval(&phone, &choice).unwrap();
        let signature = phone.sign(IMPORT_WITHDRAWAL_DOMAIN, approval.payload.as_bytes());
        assert!(verify_import_withdrawal(&choice.bond, &approval, &signature).is_ok());
        assert!(verify_import_withdrawal(&choice.bond, &approval, &approval.signature).is_err());
        assert!(
            verify_import_approval(
                &choice.bond,
                &SignedImportApproval {
                    payload: approval.payload.clone(),
                    signature: signature.clone()
                }
            )
            .is_err()
        );
        let mut changed = choice.clone();
        changed.action_id = Uuid::new_v4();
        let different = sign_import_approval(&phone, &changed).unwrap();
        assert!(verify_import_withdrawal(&choice.bond, &different, &signature).is_err());
        let other = DeviceIdentity::generate();
        assert!(
            verify_import_withdrawal(
                &choice.bond,
                &approval,
                &other.sign(IMPORT_WITHDRAWAL_DOMAIN, approval.payload.as_bytes())
            )
            .is_err()
        );
    }

    fn fixture() -> (DeviceIdentity, ImportApproval) {
        let phone = DeviceIdentity::generate();
        let pi = DeviceIdentity::generate();
        let choice = ImportApproval {
            schema_version: 1,
            bond: ImportApprovalBond {
                appliance: pi.public_identity(),
                controller: phone.public_identity(),
                root_generation: Uuid::new_v4(),
                native_generation: Uuid::new_v4(),
            },
            action_id: Uuid::new_v4(),
            action_session: Uuid::new_v4(),
            action_sequence: 1,
            source_id: "a".repeat(64),
            kind: ImportSourceKind::Volume,
            mode: ImportApprovalMode::Once,
            attachment_id: Some(Uuid::new_v4()),
            preview_id: Some(Uuid::new_v4()),
            retention: ImportRetention::Retain,
        };
        (phone, choice)
    }

    #[test]
    fn phone_identity_signs_all_three_explicit_choices() {
        let (phone, mut choice) = fixture();
        for mode in [
            ImportApprovalMode::Once,
            ImportApprovalMode::Remember,
            ImportApprovalMode::Off,
        ] {
            choice.mode = mode;
            if mode == ImportApprovalMode::Off {
                choice.attachment_id = None;
                choice.preview_id = None;
            }
            let signed = sign_import_approval(&phone, &choice).unwrap();
            assert_eq!(
                verify_import_approval(&choice.bond, &signed).unwrap(),
                choice
            );
            assert_eq!(signed.payload.as_bytes(), choice.canonical_bytes().unwrap());
        }
        assert!(sign_import_approval(&DeviceIdentity::generate(), &choice).is_err());
    }

    #[test]
    fn changed_bonds_and_rotated_keys_reject_even_a_valid_phone_signature() {
        let (phone, choice) = fixture();
        let signed = sign_import_approval(&phone, &choice).unwrap();
        let mut changes = vec![choice.bond.clone(); 6];
        changes[0].root_generation = Uuid::new_v4();
        changes[1].native_generation = Uuid::new_v4();
        changes[2].controller = DeviceIdentity::generate().public_identity();
        changes[3].appliance = DeviceIdentity::generate().public_identity();
        changes[4].controller.public_key = changes[2].controller.public_key.clone();
        changes[5].appliance.public_key = changes[3].appliance.public_key.clone();
        for bond in changes {
            assert!(verify_import_approval(&bond, &signed).is_err());
        }
    }

    #[test]
    fn every_admission_field_is_signed_and_domains_cannot_be_reused() {
        let (phone, choice) = fixture();
        let signed = sign_import_approval(&phone, &choice).unwrap();
        let mut changes = vec![choice.clone(); 8];
        changes[0].action_id = Uuid::new_v4();
        changes[1].action_session = Uuid::new_v4();
        changes[2].action_sequence += 1;
        changes[3].source_id = "b".repeat(64);
        changes[4].attachment_id = Some(Uuid::new_v4());
        changes[5].preview_id = Some(Uuid::new_v4());
        changes[6].mode = ImportApprovalMode::Remember;
        changes[7].mode = ImportApprovalMode::Off;
        changes[7].attachment_id = None;
        changes[7].preview_id = None;
        for changed in changes {
            let mut tampered = signed.clone();
            tampered.payload = String::from_utf8(changed.canonical_bytes().unwrap()).unwrap();
            assert!(verify_import_approval(&choice.bond, &tampered).is_err());
        }
        for domain in [
            b"covalent/folder-control/request/v1".as_slice(),
            b"",
            b"covalent/appliance-import/request/v2",
        ] {
            let mut wrong = signed.clone();
            wrong.signature = phone.sign(domain, signed.payload.as_bytes());
            assert!(verify_import_approval(&choice.bond, &wrong).is_err());
        }
    }

    #[test]
    fn noncanonical_unknown_duplicate_missing_and_cleanup_inputs_are_rejected_even_when_signed() {
        let (phone, choice) = fixture();
        let canonical = String::from_utf8(choice.canonical_bytes().unwrap()).unwrap();
        let variants = [
            format!(" {canonical}"),
            format!("{canonical}\n"),
            canonical.replacen("{", "{\"extra\":1,", 1),
            canonical.replacen("{", "{\"schemaVersion\":1,", 1),
            canonical.replace("\"volume\"", "\"camera\""),
            canonical.replace("\"retain\"", "\"delete\""),
            canonical.replace("\"retention\":\"retain\"", "\"retention\":true"),
            canonical.replace("\"schemaVersion\":1", "\"schemaVersion\":1.0"),
            canonical.replace("\"schemaVersion\":1", "\"schemaVersion\":2"),
        ];
        for payload in variants {
            let proof = SignedImportApproval {
                signature: phone.sign(IMPORT_APPROVAL_DOMAIN, payload.as_bytes()),
                payload,
            };
            assert!(verify_import_approval(&choice.bond, &proof).is_err());
        }
        let mut off = choice.clone();
        off.mode = ImportApprovalMode::Off;
        off.attachment_id = None;
        off.preview_id = None;
        let payload = String::from_utf8(off.canonical_bytes().unwrap())
            .unwrap()
            .replace("\"previewId\":null,", "");
        let proof = SignedImportApproval {
            signature: phone.sign(IMPORT_APPROVAL_DOMAIN, payload.as_bytes()),
            payload,
        };
        assert!(verify_import_approval(&choice.bond, &proof).is_err());
    }

    #[test]
    fn invalid_choice_bounds_and_preview_combinations_never_sign() {
        let (phone, choice) = fixture();
        let mut changes = vec![choice.clone(); 12];
        changes[0].action_id = Uuid::nil();
        changes[1].action_session = Uuid::nil();
        changes[2].action_sequence = 0;
        changes[3].action_sequence = i64::MAX as u64 + 1;
        changes[4].source_id = "A".repeat(64);
        changes[5].source_id = "../camera".into();
        changes[6].attachment_id = None;
        changes[7].preview_id = None;
        changes[8].preview_id = Some(Uuid::nil());
        changes[9].mode = ImportApprovalMode::Off;
        changes[10].bond.root_generation = Uuid::nil();
        changes[11].bond.native_generation = Uuid::nil();
        for changed in changes {
            assert!(sign_import_approval(&phone, &changed).is_err());
        }
        let mut largest = choice.clone();
        largest.action_sequence = i64::MAX as u64;
        assert!(sign_import_approval(&phone, &largest).is_ok());
    }

    #[test]
    fn malformed_signatures_weak_keys_and_oversize_payloads_fail_closed() {
        let (phone, choice) = fixture();
        let signed = sign_import_approval(&phone, &choice).unwrap();
        for signature in [
            "".into(),
            "a".repeat(86),
            format!("{}=", signed.signature),
            "!".repeat(86),
        ] {
            let mut bad = signed.clone();
            bad.signature = signature;
            assert!(verify_import_approval(&choice.bond, &bad).is_err());
        }
        let mut huge = signed.clone();
        huge.payload = " ".repeat(MAX_IMPORT_APPROVAL_BYTES + 1);
        assert!(verify_import_approval(&choice.bond, &huge).is_err());
        let mut weak = choice.bond.clone();
        let mut identity_point = [0u8; 32];
        identity_point[0] = 1;
        weak.controller = PublicIdentity::from_encoded(
            weak.controller.device_id,
            URL_SAFE_NO_PAD.encode(identity_point),
        )
        .unwrap();
        assert!(weak.validate().is_err());
        let mut forged = choice.clone();
        forged.bond = weak.clone();
        let proof = SignedImportApproval {
            payload: serde_json::to_string(&forged).unwrap(),
            signature: URL_SAFE_NO_PAD.encode([0; 64]),
        };
        assert!(verify_import_approval(&weak, &proof).is_err());
    }
}
