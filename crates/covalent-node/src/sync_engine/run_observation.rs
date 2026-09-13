//! Positive batch completion reported by the supervised rclone job.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

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
    pub failures: BTreeSet<EngineDeviceId>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_requires_the_exact_valid_index() {
        let expected = EngineIndexSnapshot {
            index_id: "0x0123456789ABCDEF".into(),
            sequence: 7,
        };
        assert!(expected.reaches(&expected));
        assert!(
            !EngineIndexSnapshot {
                sequence: 6,
                ..expected.clone()
            }
            .reaches(&expected)
        );
        assert!(
            !EngineIndexSnapshot {
                index_id: "0x1123456789ABCDEF".into(),
                ..expected.clone()
            }
            .reaches(&expected)
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
                    sequence: 0,
                }
                .is_valid()
            );
        }
    }
}
