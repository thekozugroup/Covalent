//! Fixed owner-approved import host contract. No paths, deletion or generic commands.
use covalent_core::appliance_import::{
    ImportApproval, ImportApprovalBond, ImportApprovalMode, SignedImportApproval,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImportHostError;
pub type ImportHostFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ImportHostError>> + Send + 'a>>;

/// Installed explicitly by a native host that independently verifies protected approval.
pub trait ApplianceImportHost: Send + Sync {
    fn context(&self) -> ImportHostFuture<'_, ImportContext>;
    fn action<'a>(
        &'a self,
        operation: ImportOperation,
        proof: &'a SignedImportApproval,
        withdrawal_signature: Option<&'a str>,
    ) -> ImportHostFuture<'a, ImportReceipt>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImportOperation {
    Approve,
    Lookup,
    Withdraw,
}

fn nullable<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(d)
}
fn present_boolean<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<bool>, D::Error> {
    bool::deserialize(d).map(Some)
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportContext {
    pub schema_version: u16,
    pub bond: ImportApprovalBond,
    pub volume_service_available: bool,
    #[serde(
        default,
        deserialize_with = "present_boolean",
        skip_serializing_if = "Option::is_none"
    )]
    pub camera_service_available: Option<bool>,
    pub sources_truncated: bool,
    pub sources: Vec<ImportSource>,
    #[serde(deserialize_with = "nullable")]
    pub storage: Option<ImportSpace>,
    pub withdrawal_available: bool,
    pub source_cleanup_available: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportSpace {
    pub available_after_reserve_bytes: u64,
    pub reserve_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportSource {
    pub source_id: String,
    pub kind: covalent_core::appliance_import::ImportSourceKind,
    pub name: String,
    pub connected: bool,
    pub automatic: bool,
    #[serde(
        default,
        deserialize_with = "present_boolean",
        skip_serializing_if = "Option::is_none"
    )]
    pub can_remember: Option<bool>,
    pub destination: String,
    pub preview: ImportPreview,
    pub action_session: Uuid,
    pub action_sequence: u64,
    #[serde(deserialize_with = "nullable")]
    pub confirmation: Option<ImportConfirmation>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportPreview {
    pub state: PreviewState,
    #[serde(deserialize_with = "nullable")]
    pub counts: Option<ImportCounts>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PreviewState {
    Unavailable,
    Pending,
    Scanning,
    Ready,
    Incomplete,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportCounts {
    pub eligible_files: u64,
    pub eligible_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportConfirmation {
    pub attachment_id: Uuid,
    pub preview_id: Uuid,
}

fn hex(value: &str, size: usize) -> bool {
    value.len() == size
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

impl ImportContext {
    pub fn decode(bytes: &[u8]) -> Result<Self, ImportHostError> {
        if bytes.len() > 32_768 {
            return Err(ImportHostError);
        }
        let value: Self = serde_json::from_slice(bytes).map_err(|_| ImportHostError)?;
        value.validate()?;
        Ok(value)
    }
    pub fn validate(&self) -> Result<(), ImportHostError> {
        self.bond.validate().map_err(|_| ImportHostError)?;
        if !matches!(self.schema_version, 1 | 2)
            || (self.schema_version == 2) != self.camera_service_available.is_some()
            || self.sources.len() > 24
            || !self.withdrawal_available
            || self.source_cleanup_available
        {
            return Err(ImportHostError);
        }
        if self.storage.as_ref().is_some_and(|s| {
            s.reserve_bytes != 2_147_483_648 || s.available_after_reserve_bytes > i64::MAX as u64
        }) {
            return Err(ImportHostError);
        }
        let mut ids = BTreeSet::new();
        let mut actions = BTreeMap::new();
        for s in &self.sources {
            let current = (s.action_session, s.action_sequence);
            let camera = s.kind == covalent_core::appliance_import::ImportSourceKind::Camera;
            if !hex(&s.source_id,64) || !ids.insert(&s.source_id) || s.action_session.is_nil()
                || s.action_sequence == 0 || s.action_sequence > i64::MAX as u64
                || actions.insert(s.kind,current).is_some_and(|a| a != current)
                || (self.schema_version == 1 && (camera || s.can_remember.is_some()))
                || (self.schema_version == 2 && s.can_remember.is_none())
                || (!camera && s.can_remember == Some(false))
                || (camera && s.can_remember == Some(false) && s.automatic)
                || s.name.trim().is_empty() || s.name.chars().count() > 64
                || s.name.chars().any(|c| c.is_control() || matches!(c,'\u{200b}'..='\u{200f}'|'\u{202a}'..='\u{202e}'|'\u{2060}'..='\u{206f}'|'\u{feff}'))
                || !s.destination.strip_prefix("DCIM/GPhoto/Imports/").is_some_and(|v| hex(v,16))
                || (s.connected && if camera {!self.camera_service_available.unwrap_or(false)} else {!self.volume_service_available})
                || (!s.connected && s.preview.state != PreviewState::Unavailable)
                || matches!(s.preview.state,PreviewState::Ready|PreviewState::Incomplete) != s.preview.counts.is_some() {
                return Err(ImportHostError);
            }
            if let Some(c) = &s.preview.counts
                && (c.eligible_files > 100_000
                    || c.eligible_bytes < c.eligible_files
                    || c.eligible_bytes > c.eligible_files.saturating_mul(4_294_967_296))
            {
                return Err(ImportHostError);
            }
            if let Some(c) = &s.confirmation
                && (c.attachment_id.is_nil()
                    || c.preview_id.is_nil()
                    || !s.connected
                    || s.preview.state != PreviewState::Ready
                    || !s
                        .preview
                        .counts
                        .as_ref()
                        .zip(self.storage.as_ref())
                        .is_some_and(|(c, s)| c.eligible_bytes <= s.available_after_reserve_bytes))
            {
                return Err(ImportHostError);
            }
        }
        Ok(())
    }
    /// Fresh observation for one explicit confirmation, never an address-based grant.
    pub fn admits(&self, choice: &ImportApproval) -> bool {
        self.validate().is_ok()
            && choice.canonical_bytes().is_ok()
            && choice.bond == self.bond
            && self.sources.iter().any(|s| {
                s.source_id == choice.source_id
                    && s.kind == choice.kind
                    && (choice.mode != ImportApprovalMode::Remember
                        || s.can_remember.unwrap_or(!matches!(
                            s.kind,
                            covalent_core::appliance_import::ImportSourceKind::Camera
                        )))
                    && s.action_session == choice.action_session
                    && s.action_sequence == choice.action_sequence
                    && (choice.mode == ImportApprovalMode::Off
                        || s.confirmation.as_ref().is_some_and(|c| {
                            Some(c.attachment_id) == choice.attachment_id
                                && Some(c.preview_id) == choice.preview_id
                        }))
            })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImportOutcome {
    Unknown,
    Accepted,
    Uncertain,
    Expired,
    Withdrawn,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportReceipt {
    pub action_id: Uuid,
    #[serde(deserialize_with = "nullable")]
    pub action_session: Option<Uuid>,
    #[serde(deserialize_with = "nullable")]
    pub action_sequence: Option<u64>,
    pub outcome: ImportOutcome,
    pub admission_only: bool,
    pub choice_digest: String,
}
impl ImportReceipt {
    pub fn decode(bytes: &[u8], proof: &SignedImportApproval) -> Result<Self, ImportHostError> {
        if bytes.len() > 1024 {
            return Err(ImportHostError);
        }
        let value: Self = serde_json::from_slice(bytes).map_err(|_| ImportHostError)?;
        value.validate_for(proof)?;
        Ok(value)
    }
    pub fn validate_for(&self, proof: &SignedImportApproval) -> Result<(), ImportHostError> {
        let choice: ImportApproval =
            serde_json::from_str(&proof.payload).map_err(|_| ImportHostError)?;
        if choice.canonical_bytes().map_err(|_| ImportHostError)? != proof.payload.as_bytes()
            || self.action_id != choice.action_id
            || !self.admission_only
            || self.choice_digest != format!("{:x}", Sha256::digest(proof.payload.as_bytes()))
            || if self.outcome == ImportOutcome::Unknown {
                self.action_session.is_some() || self.action_sequence.is_some()
            } else {
                self.action_session != Some(choice.action_session)
                    || self.action_sequence != Some(choice.action_sequence)
            }
        {
            return Err(ImportHostError);
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use covalent_core::{DeviceIdentity, appliance_import::*};
    #[test]
    fn camera_context_has_independent_journal_and_explicit_remember_capability() {
        let (_, mut c) = fixture();
        c.schema_version = 2;
        c.camera_service_available = Some(true);
        c.sources[0].can_remember = Some(true);
        let mut camera = c.sources[0].clone();
        camera.source_id = "c".repeat(64);
        camera.kind = ImportSourceKind::Camera;
        camera.action_session = Uuid::new_v4();
        camera.action_sequence = 99;
        camera.can_remember = Some(false);
        c.sources.push(camera);
        assert!(c.validate().is_ok());
        let mut once = choice(&c, ImportApprovalMode::Once);
        once.schema_version = 2;
        once.kind = ImportSourceKind::Camera;
        once.source_id = c.sources[1].source_id.clone();
        once.action_session = c.sources[1].action_session;
        once.action_sequence = 99;
        assert!(c.admits(&once));
        once.mode = ImportApprovalMode::Remember;
        assert!(!c.admits(&once));
        c.sources[1].can_remember = Some(true);
        assert!(c.admits(&once));
        let mut other = c.sources[1].clone();
        other.source_id = "d".repeat(64);
        other.action_session = Uuid::new_v4();
        c.sources.push(other);
        assert!(c.validate().is_err());
        c.sources.pop();
        c.volume_service_available = false;
        c.sources.remove(0);
        assert!(c.validate().is_ok() && c.admits(&once));
        c.camera_service_available = Some(false);
        assert!(c.validate().is_err());
    }
    #[test]
    fn camera_context_requires_exact_modern_fields_and_legacy_cannot_admit_camera() {
        let (_, old) = fixture();
        let value = serde_json::to_value(&old).unwrap();
        assert!(
            value.get("cameraServiceAvailable").is_none()
                && value["sources"][0].get("canRemember").is_none()
        );
        for field in ["cameraServiceAvailable", "canRemember"] {
            let mut bad = value.clone();
            if field == "canRemember" {
                bad["sources"][0][field] = serde_json::Value::Null;
            } else {
                bad[field] = serde_json::Value::Null;
            }
            assert!(ImportContext::decode(&serde_json::to_vec(&bad).unwrap()).is_err());
        }
        let mut modern = old.clone();
        modern.schema_version = 2;
        assert!(modern.validate().is_err());
        modern.camera_service_available = Some(true);
        assert!(modern.validate().is_err());
        modern.sources[0].can_remember = Some(true);
        assert!(modern.validate().is_ok());
        modern.sources[0].kind = ImportSourceKind::Camera;
        modern.schema_version = 1;
        assert!(modern.validate().is_err());
    }
    pub(crate) fn fixture() -> (DeviceIdentity, ImportContext) {
        let phone = DeviceIdentity::generate();
        let context = ImportContext {
            schema_version: 1,
            bond: ImportApprovalBond {
                appliance: DeviceIdentity::generate().public_identity(),
                controller: phone.public_identity(),
                root_generation: Uuid::new_v4(),
                native_generation: Uuid::new_v4(),
            },
            volume_service_available: true,
            camera_service_available: None,
            sources_truncated: false,
            sources: vec![ImportSource {
                source_id: "a".repeat(64),
                kind: ImportSourceKind::Volume,
                name: "Example SD card".into(),
                connected: true,
                automatic: false,
                can_remember: None,
                destination: format!("DCIM/GPhoto/Imports/{}", "b".repeat(16)),
                preview: ImportPreview {
                    state: PreviewState::Ready,
                    counts: Some(ImportCounts {
                        eligible_files: 1,
                        eligible_bytes: 5,
                    }),
                },
                action_session: Uuid::new_v4(),
                action_sequence: 1,
                confirmation: Some(ImportConfirmation {
                    attachment_id: Uuid::new_v4(),
                    preview_id: Uuid::new_v4(),
                }),
            }],
            storage: Some(ImportSpace {
                available_after_reserve_bytes: 1024,
                reserve_bytes: 2_147_483_648,
            }),
            withdrawal_available: true,
            source_cleanup_available: false,
        };
        (phone, context)
    }
    pub(crate) fn choice(context: &ImportContext, mode: ImportApprovalMode) -> ImportApproval {
        let source = &context.sources[0];
        let preview = source.confirmation.as_ref();
        ImportApproval {
            schema_version: if source.kind == ImportSourceKind::Camera {
                2
            } else {
                1
            },
            bond: context.bond.clone(),
            action_id: Uuid::new_v4(),
            action_session: source.action_session,
            action_sequence: source.action_sequence,
            source_id: source.source_id.clone(),
            kind: source.kind,
            mode,
            attachment_id: if mode == ImportApprovalMode::Off {
                None
            } else {
                Some(preview.unwrap().attachment_id)
            },
            preview_id: if mode == ImportApprovalMode::Off {
                None
            } else {
                Some(preview.unwrap().preview_id)
            },
            retention: ImportRetention::Retain,
        }
    }
    pub(crate) fn receipt(proof: &SignedImportApproval, outcome: ImportOutcome) -> ImportReceipt {
        let c: ImportApproval = serde_json::from_str(&proof.payload).unwrap();
        ImportReceipt {
            action_id: c.action_id,
            action_session: if outcome == ImportOutcome::Unknown {
                None
            } else {
                Some(c.action_session)
            },
            action_sequence: if outcome == ImportOutcome::Unknown {
                None
            } else {
                Some(c.action_sequence)
            },
            outcome,
            admission_only: true,
            choice_digest: format!("{:x}", Sha256::digest(proof.payload.as_bytes())),
        }
    }
    #[test]
    fn context_requires_complete_fresh_bounded_confirmation_and_safe_capabilities() {
        let (_, context) = fixture();
        let original = serde_json::to_value(&context).unwrap();
        assert!(ImportContext::decode(&serde_json::to_vec(&original).unwrap()).is_ok());
        for edit in [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11] {
            let mut v = original.clone();
            match edit {
                0 => v["sourceCleanupAvailable"] = true.into(),
                1 => v["withdrawalAvailable"] = false.into(),
                2 => v["sources"][0]["actionSequence"] = 0.into(),
                3 => v["sources"][0]["connected"] = false.into(),
                4 => v["sources"][0]["preview"]["counts"]["eligibleBytes"] = 0.into(),
                5 => v["sources"][0]["destination"] = "/private/path".into(),
                6 => v["sources"][0]["preview"]["state"] = "incomplete".into(),
                7 => v["storage"] = serde_json::Value::Null,
                8 => v["sources"][0]["confirmation"]["previewId"] = Uuid::nil().to_string().into(),
                9 => v["sources"][0]["name"] = "\u{202e}unsafe".into(),
                10 => v["sources"][0]["unexpected"] = true.into(),
                _ => {
                    v.as_object_mut().unwrap().remove("storage");
                }
            };
            assert!(
                ImportContext::decode(&serde_json::to_vec(&v).unwrap()).is_err(),
                "case {edit}"
            );
        }
        let mut c = choice(&context, ImportApprovalMode::Once);
        assert!(context.admits(&c));
        c.action_sequence += 1;
        assert!(!context.admits(&c));
        let mut partial = context.clone();
        partial.sources[0].confirmation = None;
        partial.sources[0].preview.state = PreviewState::Incomplete;
        assert!(partial.validate().is_ok());
        assert!(!partial.admits(&choice(&context, ImportApprovalMode::Remember)));
        assert!(partial.admits(&choice(&partial, ImportApprovalMode::Off)));
    }
    #[test]
    fn receipts_bind_the_exact_payload_and_never_claim_cloud_completion() {
        let (phone, c) = fixture();
        let proof = sign_import_approval(&phone, &choice(&c, ImportApprovalMode::Once)).unwrap();
        for outcome in [
            ImportOutcome::Unknown,
            ImportOutcome::Accepted,
            ImportOutcome::Uncertain,
            ImportOutcome::Expired,
            ImportOutcome::Withdrawn,
        ] {
            assert!(receipt(&proof, outcome).validate_for(&proof).is_ok());
        }
        let r = receipt(&proof, ImportOutcome::Accepted);
        let mut wrong = r.clone();
        wrong.admission_only = false;
        assert!(wrong.validate_for(&proof).is_err());
        wrong = r.clone();
        wrong.choice_digest = "0".repeat(64);
        assert!(wrong.validate_for(&proof).is_err());
        wrong = r.clone();
        wrong.action_sequence = Some(2);
        assert!(wrong.validate_for(&proof).is_err());
        let unknown = receipt(&proof, ImportOutcome::Unknown);
        let mut wrong = unknown.clone();
        wrong.action_session = Some(Uuid::new_v4());
        assert!(wrong.validate_for(&proof).is_err());
    }
}
