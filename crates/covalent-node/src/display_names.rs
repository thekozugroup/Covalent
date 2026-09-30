//! Local presentation preferences. These never change signed names or authority.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::PathBuf;
use std::sync::Mutex;

use covalent_core::CoreError;
use covalent_protocol::DeviceId;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const MAX_BYTES: u64 = 128 * 1_024;

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DisplayNames {
    pub peers: BTreeMap<DeviceId, String>,
    pub folders: BTreeMap<Uuid, String>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredNames {
    schema_version: u16,
    names: DisplayNames,
}

#[derive(Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub(crate) enum DisplayNameRequest {
    Peer {
        peer_id: DeviceId,
        #[serde(deserialize_with = "required_nullable_name")]
        name: Option<String>,
    },
    Folder {
        folder_id: Uuid,
        #[serde(deserialize_with = "required_nullable_name")]
        name: Option<String>,
    },
}

fn required_nullable_name<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(deserializer)
}

pub(crate) struct DisplayNameStore {
    path: PathBuf,
    names: Mutex<DisplayNames>,
}

fn invalid() -> CoreError {
    CoreError::InvalidState("invalid local display name".to_owned())
}

fn valid_name(name: &str, maximum: usize) -> bool {
    !name.trim().is_empty() && name.len() <= maximum && !name.chars().any(char::is_control)
}

impl DisplayNames {
    fn validate(&self) -> Result<(), CoreError> {
        if self.peers.len() > 128
            || self.folders.len() > 256
            || self.peers.values().any(|name| !valid_name(name, 80))
            || self
                .folders
                .iter()
                .any(|(id, name)| id.is_nil() || !valid_name(name, 256))
        {
            return Err(invalid());
        }
        Ok(())
    }
}

impl DisplayNameStore {
    pub fn open(path: PathBuf) -> Result<Self, CoreError> {
        let names = match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => DisplayNames::default(),
            Err(source) => {
                return Err(CoreError::Io {
                    operation: "inspect local display names",
                    path,
                    source,
                });
            }
            Ok(metadata) => {
                if !metadata.is_file()
                    || metadata.file_type().is_symlink()
                    || metadata.len() > MAX_BYTES
                {
                    return Err(invalid());
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt as _;
                    if metadata.permissions().mode() & 0o077 != 0 {
                        return Err(invalid());
                    }
                }
                let file = std::fs::File::open(&path).map_err(|source| CoreError::Io {
                    operation: "open local display names",
                    path: path.clone(),
                    source,
                })?;
                let mut bytes = Vec::new();
                file.take(MAX_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|source| CoreError::Io {
                        operation: "read local display names",
                        path: path.clone(),
                        source,
                    })?;
                if bytes.len() as u64 > MAX_BYTES {
                    return Err(invalid());
                }
                let stored: StoredNames = serde_json::from_slice(&bytes)?;
                if stored.schema_version != 1 {
                    return Err(invalid());
                }
                stored.names.validate()?;
                stored.names
            }
        };
        Ok(Self {
            path,
            names: Mutex::new(names),
        })
    }

    pub fn snapshot(&self) -> Result<DisplayNames, CoreError> {
        self.names
            .lock()
            .map(|names| names.clone())
            .map_err(|_| CoreError::Synchronization)
    }

    pub fn set(&self, request: DisplayNameRequest) -> Result<(), CoreError> {
        let mut current = self.names.lock().map_err(|_| CoreError::Synchronization)?;
        let mut names = current.clone();
        match request {
            DisplayNameRequest::Peer { peer_id, name } => match name {
                Some(name) => {
                    names.peers.insert(peer_id, name);
                }
                None => {
                    names.peers.remove(&peer_id);
                }
            },
            DisplayNameRequest::Folder { folder_id, name } => match name {
                Some(name) => {
                    names.folders.insert(folder_id, name);
                }
                None => {
                    names.folders.remove(&folder_id);
                }
            },
        }
        names.validate()?;
        let bytes = serde_json::to_vec(&StoredNames {
            schema_version: 1,
            names: names.clone(),
        })?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(invalid());
        }
        crate::persist_private_file(&self.path, &bytes)?;
        *current = names;
        Ok(())
    }
}
