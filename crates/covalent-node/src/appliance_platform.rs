//! Bounded, read-only observations supplied explicitly by an appliance host.

use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, future::Future, pin::Pin};

/// A fixed failure: no platform path, command output or credential is exposed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlatformReadError;

/// The native host owns its platform IPC. This hook grants no mutating operation.
pub trait AppliancePlatformReader: Send + Sync {
    fn read(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<PlatformSnapshot, PlatformReadError>> + Send + '_>>;
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlatformSnapshot {
    schema_version: u16,
    volume_service_available: bool,
    camera_service_available: bool,
    sources_truncated: bool,
    sources: Vec<ImportSource>,
    #[serde(deserialize_with = "required_nullable")]
    storage: Option<StorageSpace>,
    #[serde(deserialize_with = "required_nullable")]
    updates: Option<UpdateState>,
    import_approval_available: bool,
    source_cleanup_available: bool,
}

fn required_nullable<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(d)
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StorageSpace {
    available_after_reserve_bytes: u64,
    reserve_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateState {
    configured: bool,
    automatic: bool,
    busy: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ImportSource {
    source_id: String,
    kind: SourceKind,
    name: String,
    connected: bool,
    automatic: bool,
    destination: String,
    preview: Preview,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum SourceKind {
    Volume,
    Camera,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Preview {
    state: PreviewState,
    #[serde(deserialize_with = "required_nullable")]
    counts: Option<PreviewCounts>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum PreviewState {
    Unavailable,
    Pending,
    Scanning,
    Ready,
    Incomplete,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PreviewCounts {
    eligible_files: u64,
    eligible_bytes: u64,
}

impl PlatformSnapshot {
    /// Strict decode at the host boundary. Counts describe a cached scan, never consent.
    pub fn decode(bytes: &[u8]) -> Result<Self, PlatformReadError> {
        if bytes.len() > 32_768 {
            return Err(PlatformReadError);
        }
        let value: Self = serde_json::from_slice(bytes).map_err(|_| PlatformReadError)?;
        value.validate()?;
        Ok(value)
    }

    pub(crate) fn validate(&self) -> Result<(), PlatformReadError> {
        let mut ids = BTreeSet::new();
        if self.schema_version != 1
            || self.sources.len() > 24
            || self.import_approval_available
            || self.source_cleanup_available
        {
            return Err(PlatformReadError);
        }
        if let Some(space) = &self.storage
            && (space.reserve_bytes != 2 * 1024 * 1024 * 1024
                || space.available_after_reserve_bytes > i64::MAX as u64)
        {
            return Err(PlatformReadError);
        }
        for source in &self.sources {
            let service = match source.kind {
                SourceKind::Volume => self.volume_service_available,
                SourceKind::Camera => self.camera_service_available,
            };
            let suffix = source.destination.strip_prefix("DCIM/GPhoto/Imports/");
            let valid_hex = |text: &str, len| {
                text.len() == len
                    && text
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            };
            if !valid_hex(&source.source_id, 64) || !ids.insert(&source.source_id)
                || source.name.trim().is_empty() || source.name.chars().count() > 64
                || source.name.chars().any(|c| c.is_control() || matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}'))
                || !suffix.is_some_and(|s| valid_hex(s, 16)) || (source.connected && !service)
                || (!source.connected && source.preview.state != PreviewState::Unavailable) {
                return Err(PlatformReadError);
            }
            let counted = matches!(
                source.preview.state,
                PreviewState::Ready | PreviewState::Incomplete
            );
            if counted != source.preview.counts.is_some() {
                return Err(PlatformReadError);
            }
            if let Some(counts) = &source.preview.counts
                && (counts.eligible_files > 100_000
                    || counts.eligible_bytes
                        > counts.eligible_files.saturating_mul(4 * 1024 * 1024 * 1024)
                    || (counts.eligible_files > 0 && counts.eligible_bytes < counts.eligible_files))
            {
                return Err(PlatformReadError);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    pub(crate) fn fixture() -> serde_json::Value {
        // Exact output of the Pi Java projection, also decoded by the Android JVM tests.
        serde_json::from_slice(include_bytes!(
            "../../../apps/android/app/src/test/resources/appliance-platform.json"
        ))
        .unwrap()
    }
    #[test]
    fn strict_platform_contract_rejects_forged_authority_paths_types_counts_and_stale_data() {
        assert!(PlatformSnapshot::decode(&serde_json::to_vec(&fixture()).unwrap()).is_ok());
        let mut bad = Vec::new();
        for (key, value) in [
            ("schemaVersion", serde_json::json!(2)),
            ("importApprovalAvailable", serde_json::json!(true)),
            ("sourceCleanupAvailable", serde_json::json!(true)),
            ("cameraServiceAvailable", serde_json::json!("true")),
            ("token", serde_json::json!("secret")),
        ] {
            let mut x = fixture();
            x[key] = value;
            bad.push(x);
        }
        for (key, value) in [
            ("sourceId", serde_json::json!("raw-volume-uuid")),
            ("kind", serde_json::json!("disk")),
            ("name", serde_json::json!("bad\u{202e}name")),
            ("destination", serde_json::json!("/data/private")),
            ("connected", serde_json::json!(false)),
            ("automatic", serde_json::json!(1)),
        ] {
            let mut x = fixture();
            x["sources"][0][key] = value;
            bad.push(x);
        }
        for (key, value) in [
            ("eligibleFiles", serde_json::json!(-1)),
            ("eligibleFiles", serde_json::json!(100001)),
            ("eligibleFiles", serde_json::json!("7")),
            ("eligibleBytes", serde_json::json!(1)),
            ("eligibleBytes", serde_json::json!(1.5)),
            ("eligibleBytes", serde_json::json!(40000000000_u64)),
        ] {
            let mut x = fixture();
            x["sources"][0]["preview"]["counts"][key] = value;
            bad.push(x);
        }
        let mut x = fixture();
        x["sources"][0]["preview"]["state"] = serde_json::json!("scanning");
        bad.push(x);
        let mut x = fixture();
        x["volumeServiceAvailable"] = serde_json::json!(false);
        bad.push(x);
        let mut x = fixture();
        x["storage"]["reserveBytes"] = serde_json::json!(0);
        bad.push(x);
        let mut x = fixture();
        x.as_object_mut().unwrap().remove("updates");
        bad.push(x);
        let mut x = fixture();
        let duplicate = x["sources"][0].clone();
        x["sources"].as_array_mut().unwrap().push(duplicate);
        bad.push(x);
        let mut x = fixture();
        x["sources"] = serde_json::json!(vec![x["sources"][0].clone(); 25]);
        bad.push(x);
        for x in bad {
            assert!(
                PlatformSnapshot::decode(&serde_json::to_vec(&x).unwrap()).is_err(),
                "accepted malformed platform response"
            );
        }
        assert!(PlatformSnapshot::decode(&vec![b' '; 32769]).is_err());
    }
    #[test]
    fn absent_observations_and_disconnected_sources_never_invent_zero_counts() {
        let mut x = fixture();
        x["storage"] = serde_json::Value::Null;
        x["updates"] = serde_json::Value::Null;
        x["sources"][0]["connected"] = serde_json::json!(false);
        x["sources"][0]["preview"] = serde_json::json!({"state":"unavailable","counts":null});
        let result = PlatformSnapshot::decode(&serde_json::to_vec(&x).unwrap()).unwrap();
        assert!(
            result.storage.is_none()
                && result.updates.is_none()
                && result.sources[0].preview.counts.is_none()
        );
    }
}
