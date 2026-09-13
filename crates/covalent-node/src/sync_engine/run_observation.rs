//! Positive batch completion from the maintained worker, never from raw Need.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::EngineSessionError;
use super::config::EngineDeviceId;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EngineIndexSnapshot {
    pub index_id: String,
    pub sequence: u64,
}

impl EngineIndexSnapshot {
    pub fn is_valid(&self) -> bool {
        let bytes = self.index_id.as_bytes();
        bytes.len() == 18
            && bytes.starts_with(b"0x")
            && bytes[2..]
                .iter()
                .all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(b))
            && bytes[2..].iter().any(|b| *b != b'0')
            && self.sequence <= i64::MAX as u64
    }

    pub fn reaches(&self, expected: &Self) -> bool {
        self.is_valid()
            && expected.is_valid()
            && self.index_id == expected.index_id
            && self.sequence >= expected.sequence
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct EngineRunObservation {
    pub local_index: Option<EngineIndexSnapshot>,
    pub completions: BTreeMap<EngineDeviceId, EngineIndexSnapshot>,
}

impl EngineRunObservation {
    pub(super) fn parse(value: Value) -> Result<Self, EngineSessionError> {
        let invalid = EngineSessionError::EngineUnavailable;
        let state = value.get("state").and_then(Value::as_str).ok_or(invalid)?;
        let errors = value.get("errors").and_then(Value::as_u64).ok_or(invalid)?;
        let watch_error = value
            .get("watchError")
            .and_then(Value::as_str)
            .ok_or(invalid)?;
        let error = match value.get("error") {
            None => "",
            Some(value) => value.as_str().ok_or(invalid)?,
        };
        // A valid but busy/unhealthy observation carries no completion claim.
        if state != "idle" || errors != 0 || !watch_error.is_empty() || !error.is_empty() {
            return Ok(Self::default());
        }
        let index_id = value
            .get("covalentLocalIndexID")
            .and_then(Value::as_str)
            .ok_or(invalid)?;
        let sequence = value
            .get("sequence")
            .and_then(Value::as_u64)
            .ok_or(invalid)?;
        let local_index = if index_id.is_empty() {
            None
        } else {
            let index = EngineIndexSnapshot {
                index_id: index_id.to_owned(),
                sequence,
            };
            if !index.is_valid() {
                return Err(invalid);
            }
            Some(index)
        };
        let rows = value
            .get("covalentCompletion")
            .and_then(Value::as_object)
            .ok_or(invalid)?;
        if rows.len() > 128 {
            return Err(invalid);
        }
        let mut completions = BTreeMap::new();
        for (device, row) in rows {
            let device = EngineDeviceId::parse(device).map_err(|_| invalid)?;
            let index = EngineIndexSnapshot {
                index_id: row
                    .get("indexID")
                    .and_then(Value::as_str)
                    .ok_or(invalid)?
                    .to_owned(),
                sequence: row.get("sequence").and_then(Value::as_u64).ok_or(invalid)?,
            };
            if !index.is_valid() {
                return Err(invalid);
            }
            completions.insert(device, index);
        }
        Ok(Self {
            local_index,
            completions,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn exact_index_and_positive_worker_proof_are_required_for_batch_completion() {
        let device = EngineDeviceId::from_certificate_der(b"run observation fixture").unwrap();
        let expected = EngineIndexSnapshot {
            index_id: "0x0123456789ABCDEF".into(),
            sequence: 7,
        };
        let status = json!({
            "state": "idle", "errors": 0, "watchError": "", "sequence": 11,
            "needFiles": 3, "needBytes": 90,
            "covalentLocalIndexID": "0xFEDCBA9876543210",
            "covalentCompletion": {device.as_str(): {"indexID": expected.index_id, "sequence": 7}}
        });
        let observed = EngineRunObservation::parse(status.clone()).unwrap();
        assert!(observed.completions[&device].reaches(&expected));
        assert!(
            !observed.completions[&device].reaches(&EngineIndexSnapshot {
                sequence: 8,
                ..expected.clone()
            })
        );
        assert!(
            !observed.completions[&device].reaches(&EngineIndexSnapshot {
                index_id: "0x1123456789ABCDEF".into(),
                ..expected.clone()
            })
        );
        for (field, value) in [
            ("state", json!("scanning")),
            ("errors", json!(1)),
            ("watchError", json!("failed")),
            ("error", json!("failed")),
        ] {
            let mut changed = status.clone();
            changed[field] = value;
            assert_eq!(
                EngineRunObservation::parse(changed).unwrap(),
                EngineRunObservation::default()
            );
        }
        let mut missing = status.clone();
        missing
            .as_object_mut()
            .unwrap()
            .remove("covalentCompletion");
        assert!(EngineRunObservation::parse(missing).is_err());
        let mut stale = status.clone();
        stale["covalentCompletion"] = json!({});
        assert!(
            EngineRunObservation::parse(stale)
                .unwrap()
                .completions
                .is_empty()
        );
        for id in [
            "0x0000000000000000",
            "0x0123456789abcdef",
            "0123456789ABCDEF",
            "0x1",
        ] {
            assert!(
                !EngineIndexSnapshot {
                    index_id: id.into(),
                    sequence: 0
                }
                .is_valid()
            );
        }
        let mut malformed = status;
        malformed["sequence"] = json!(-1);
        assert!(EngineRunObservation::parse(malformed).is_err());
    }
}
