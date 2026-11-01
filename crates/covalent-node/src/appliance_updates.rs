//! Fixed privileged update-policy bridge. Credentials and installation stay local.
use covalent_core::{
    appliance_import::ImportApprovalBond,
    appliance_updates::{AutomaticUpdateChange, SignedAutomaticUpdateChange},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{future::Future, pin::Pin};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UpdateHostError;
pub type UpdateHostFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, UpdateHostError>> + Send + 'a>>;

/// The host independently verifies its protected bond and durable revision under
/// the same lock as policy publication. Node admission never substitutes for it.
pub trait ApplianceUpdateHost: Send + Sync {
    fn context(&self) -> UpdateHostFuture<'_, AutomaticUpdateContext>;
    fn action<'a>(
        &'a self,
        operation: UpdateOperation,
        proof: &'a SignedAutomaticUpdateChange,
    ) -> UpdateHostFuture<'a, AutomaticUpdateReceipt>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateOperation {
    Apply,
    Lookup,
}

fn nullable<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Uuid>, D::Error> {
    Option::<Uuid>::deserialize(d)
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AutomaticUpdateContext {
    pub schema_version: u16,
    pub bond: ImportApprovalBond,
    pub configured: bool,
    pub enabled: bool,
    #[serde(deserialize_with = "nullable")]
    pub revision: Option<Uuid>,
    pub busy: bool,
}
impl AutomaticUpdateContext {
    pub fn decode(bytes: &[u8]) -> Result<Self, UpdateHostError> {
        if bytes.len() > 2048 {
            return Err(UpdateHostError);
        }
        let value: Self = serde_json::from_slice(bytes).map_err(|_| UpdateHostError)?;
        value.validate()?;
        Ok(value)
    }
    pub fn validate(&self) -> Result<(), UpdateHostError> {
        self.bond.validate().map_err(|_| UpdateHostError)?;
        if self.schema_version != 1
            || self.configured != self.revision.is_some()
            || self.revision.is_some_and(|r| r.is_nil())
            || (!self.configured && self.enabled)
        {
            return Err(UpdateHostError);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UpdateOutcome {
    Applied,
    /// An observation, never cancellation or permission to resend a mutation.
    NotObserved,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AutomaticUpdateReceipt {
    pub schema_version: u16,
    pub state: UpdateOutcome,
    pub action_id: Uuid,
    pub payload_digest: String,
    pub revision: Uuid,
    pub enabled: bool,
}
impl AutomaticUpdateReceipt {
    pub fn decode(
        bytes: &[u8],
        proof: &SignedAutomaticUpdateChange,
    ) -> Result<Self, UpdateHostError> {
        if bytes.len() > 1024 {
            return Err(UpdateHostError);
        }
        let value: Self = serde_json::from_slice(bytes).map_err(|_| UpdateHostError)?;
        value.validate_for(proof)?;
        Ok(value)
    }
    pub fn validate_for(&self, proof: &SignedAutomaticUpdateChange) -> Result<(), UpdateHostError> {
        if proof.payload.len() > covalent_core::appliance_updates::MAX_AUTOMATIC_UPDATE_CHANGE_BYTES
        {
            return Err(UpdateHostError);
        }
        let choice: AutomaticUpdateChange =
            serde_json::from_str(&proof.payload).map_err(|_| UpdateHostError)?;
        if choice.canonical_bytes().map_err(|_| UpdateHostError)? != proof.payload.as_bytes()
            || self.schema_version != 1
            || self.action_id != choice.action_id
            || self.revision.is_nil()
            || self.payload_digest != format!("{:x}", Sha256::digest(proof.payload.as_bytes()))
            || (self.state == UpdateOutcome::Applied
                && (self.enabled != choice.enabled || self.revision == choice.expected_revision))
            || (self.state == UpdateOutcome::NotObserved
                && self.revision != choice.expected_revision)
        {
            return Err(UpdateHostError);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use covalent_core::{DeviceIdentity, appliance_updates::sign_automatic_update_change};
    use serde_json::{Value, json};

    pub(crate) fn fixture() -> (
        DeviceIdentity,
        AutomaticUpdateContext,
        SignedAutomaticUpdateChange,
    ) {
        let phone = DeviceIdentity::generate();
        let context = AutomaticUpdateContext {
            schema_version: 1,
            bond: ImportApprovalBond {
                appliance: DeviceIdentity::generate().public_identity(),
                controller: phone.public_identity(),
                root_generation: Uuid::new_v4(),
                native_generation: Uuid::new_v4(),
            },
            configured: true,
            enabled: false,
            revision: Some(Uuid::new_v4()),
            busy: false,
        };
        let proof = sign_automatic_update_change(
            &phone,
            &AutomaticUpdateChange {
                schema_version: 1,
                bond: context.bond.clone(),
                action_id: Uuid::new_v4(),
                expected_revision: context.revision.unwrap(),
                enabled: true,
            },
        )
        .unwrap();
        (phone, context, proof)
    }
    #[test]
    fn context_requires_a_valid_full_bond_and_explicit_configured_revision() {
        let (_, context, _) = fixture();
        let valid = serde_json::to_vec(&context).unwrap();
        assert_eq!(AutomaticUpdateContext::decode(&valid).unwrap(), context);
        let mut unconfigured = context.clone();
        unconfigured.configured = false;
        unconfigured.revision = None;
        assert!(unconfigured.validate().is_ok());
        unconfigured.enabled = true;
        assert!(unconfigured.validate().is_err());
        let value = serde_json::to_value(context).unwrap();
        for (key, replacement) in [
            ("schemaVersion", json!(2)),
            ("configured", json!(false)),
            ("revision", Value::Null),
            ("revision", json!(Uuid::nil())),
            ("busy", Value::Null),
            ("token", json!("must-never-cross-the-bridge")),
        ] {
            let mut wrong = value.clone();
            wrong[key] = replacement;
            assert!(AutomaticUpdateContext::decode(&serde_json::to_vec(&wrong).unwrap()).is_err());
        }
        for key in value.as_object().unwrap().keys() {
            let mut wrong = value.clone();
            wrong.as_object_mut().unwrap().remove(key);
            assert!(AutomaticUpdateContext::decode(&serde_json::to_vec(&wrong).unwrap()).is_err());
        }
        let mut duplicate = String::from_utf8(valid).unwrap();
        duplicate.insert_str(1, "\"schemaVersion\":1,");
        assert!(AutomaticUpdateContext::decode(duplicate.as_bytes()).is_err());
        assert!(AutomaticUpdateContext::decode(&[b' '; 2049]).is_err());
    }
    #[test]
    fn receipts_bind_the_exact_choice_and_distinguish_unobserved_from_applied() {
        let (_, context, proof) = fixture();
        let choice: AutomaticUpdateChange = serde_json::from_str(&proof.payload).unwrap();
        let receipt = AutomaticUpdateReceipt {
            schema_version: 1,
            state: UpdateOutcome::Applied,
            action_id: choice.action_id,
            payload_digest: format!("{:x}", Sha256::digest(proof.payload.as_bytes())),
            revision: Uuid::new_v4(),
            enabled: true,
        };
        let bytes = serde_json::to_vec(&receipt).unwrap();
        assert_eq!(
            AutomaticUpdateReceipt::decode(&bytes, &proof).unwrap(),
            receipt
        );
        let value = serde_json::to_value(&receipt).unwrap();
        for (key, replacement) in [
            ("schemaVersion", json!(2)),
            ("actionId", json!(Uuid::new_v4())),
            ("payloadDigest", json!("a".repeat(64))),
            ("revision", json!(Uuid::nil())),
            ("revision", json!(choice.expected_revision)),
            ("enabled", json!(false)),
            ("state", json!("notObserved")),
            ("token", json!("private")),
        ] {
            let mut wrong = value.clone();
            wrong[key] = replacement;
            assert!(
                AutomaticUpdateReceipt::decode(&serde_json::to_vec(&wrong).unwrap(), &proof)
                    .is_err()
            );
        }
        for key in value.as_object().unwrap().keys() {
            let mut wrong = value.clone();
            wrong.as_object_mut().unwrap().remove(key);
            assert!(
                AutomaticUpdateReceipt::decode(&serde_json::to_vec(&wrong).unwrap(), &proof)
                    .is_err()
            );
        }
        let mut duplicate = String::from_utf8(bytes).unwrap();
        duplicate.insert_str(1, "\"enabled\":true,");
        assert!(AutomaticUpdateReceipt::decode(duplicate.as_bytes(), &proof).is_err());
        assert!(AutomaticUpdateReceipt::decode(&[b' '; 1025], &proof).is_err());
        let observation = AutomaticUpdateReceipt {
            state: UpdateOutcome::NotObserved,
            revision: choice.expected_revision,
            enabled: context.enabled,
            ..receipt
        };
        assert!(observation.validate_for(&proof).is_ok());
        let altered = SignedAutomaticUpdateChange {
            payload: proof
                .payload
                .replace("\"enabled\":true", "\"enabled\":false"),
            ..proof
        };
        assert!(observation.validate_for(&altered).is_err());
    }
}
