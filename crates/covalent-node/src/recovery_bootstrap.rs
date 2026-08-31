use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::net::SocketAddr;
use std::path::Path;

use covalent_core::{CoreError, Engine};
use covalent_protocol::{DeviceId, PeerRole};
use serde::{Deserialize, Serialize};

const PROVIDER_CONNECTION_SCHEMA_VERSION: u16 = 1;
const MAX_RECOVERED_PROVIDERS: usize = 128;

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProviderConnection {
    peer_id: DeviceId,
    address: SocketAddr,
    certificate_der: String,
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProviderConnectionState {
    schema_version: u16,
    providers: BTreeMap<DeviceId, ProviderConnection>,
}

/// Reconstructs the existing node provider-state contract exclusively from
/// owner-signed recovery-kit transports already authenticated by the engine.
pub(super) fn persist_recovered_provider_connections(
    engine: &Engine,
    path: &Path,
) -> Result<(), CoreError> {
    let config = engine.config()?;
    let mut providers = BTreeMap::new();
    for (peer_id, grant) in config.trusted_peers {
        if grant.revoked || !grant.roles.contains(&PeerRole::StorageProvider) {
            continue;
        }
        let transport = engine.trusted_peer_transport(peer_id, PeerRole::StorageProvider)?;
        let address = transport.address.parse::<SocketAddr>().map_err(|_| {
            CoreError::InvalidState("recovered provider address is invalid".to_owned())
        })?;
        providers.insert(
            peer_id,
            ProviderConnection {
                peer_id,
                address,
                certificate_der: transport.certificate_der,
            },
        );
    }
    if providers.len() > MAX_RECOVERED_PROVIDERS {
        return Err(CoreError::ResourceLimit("recovered provider connections"));
    }
    let bytes = serde_json::to_vec_pretty(&ProviderConnectionState {
        schema_version: PROVIDER_CONNECTION_SCHEMA_VERSION,
        providers,
    })?;
    persist_private_noclobber(path, &bytes)
}

fn persist_private_noclobber(path: &Path, bytes: &[u8]) -> Result<(), CoreError> {
    let parent = path.parent().ok_or_else(|| {
        CoreError::InvalidState("provider connection path has no parent".to_owned())
    })?;
    if let Some(existing) = read_private_bounded_optional(path, bytes.len().max(1) as u64)? {
        return if existing == bytes {
            Ok(())
        } else {
            Err(CoreError::InvalidState(
                "recovered provider connections conflict with existing state".to_owned(),
            ))
        };
    }
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|source| CoreError::Io {
            operation: "stage recovered provider connections",
            path: path.to_path_buf(),
            source,
        })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|source| CoreError::Io {
                operation: "protect recovered provider connections",
                path: path.to_path_buf(),
                source,
            })?;
    }
    temporary
        .write_all(bytes)
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|source| CoreError::Io {
            operation: "sync recovered provider connections",
            path: path.to_path_buf(),
            source,
        })?;
    provider_bootstrap_failpoint()?;
    if let Err(error) = temporary.persist_noclobber(path) {
        drop(error.file);
        if let Some(existing) = read_private_bounded_optional(path, bytes.len().max(1) as u64)?
            && existing == bytes
        {
            return Ok(());
        }
        return Err(CoreError::Io {
            operation: "commit recovered provider connections",
            path: path.to_path_buf(),
            source: error.error,
        });
    }
    #[cfg(unix)]
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| CoreError::Io {
            operation: "sync recovered provider connection directory",
            path: parent.to_path_buf(),
            source,
        })?;
    Ok(())
}

fn read_private_bounded_optional(path: &Path, maximum: u64) -> Result<Option<Vec<u8>>, CoreError> {
    use std::io::Read as _;

    #[cfg(unix)]
    let (file, length) = {
        use rustix::fs::{FileType, Mode, OFlags, fstat, open};

        let descriptor = match open(
            path,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        ) {
            Ok(descriptor) => descriptor,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(error) => {
                return Err(CoreError::Io {
                    operation: "open recovered provider connections without following links",
                    path: path.to_path_buf(),
                    source: std::io::Error::from_raw_os_error(error.raw_os_error()),
                });
            }
        };
        let stat = fstat(&descriptor).map_err(|error| CoreError::Io {
            operation: "inspect recovered provider connection handle",
            path: path.to_path_buf(),
            source: std::io::Error::from_raw_os_error(error.raw_os_error()),
        })?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
            || stat.st_mode & 0o077 != 0
            || stat.st_size < 0
            || stat.st_size as u64 > maximum
        {
            return Err(CoreError::InvalidState(
                "recovered provider connections are not a bounded private regular file".to_owned(),
            ));
        }
        (fs::File::from(descriptor), stat.st_size as u64)
    };
    #[cfg(not(unix))]
    let (file, length) = {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(CoreError::Io {
                    operation: "inspect recovered provider connections",
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > maximum {
            return Err(CoreError::InvalidState(
                "recovered provider connections are not a bounded regular file".to_owned(),
            ));
        }
        let file = fs::File::open(path).map_err(|source| CoreError::Io {
            operation: "open recovered provider connections",
            path: path.to_path_buf(),
            source,
        })?;
        (file, metadata.len())
    };
    let mut bytes = Vec::with_capacity(length as usize);
    file.take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| CoreError::Io {
            operation: "read recovered provider connections",
            path: path.to_path_buf(),
            source,
        })?;
    if bytes.len() as u64 > maximum {
        return Err(CoreError::ResourceLimit(
            "recovered provider connection state",
        ));
    }
    Ok(Some(bytes))
}

#[cfg(test)]
thread_local! {
    static PROVIDER_BOOTSTRAP_FAILPOINT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn provider_bootstrap_failpoint() -> Result<(), CoreError> {
    PROVIDER_BOOTSTRAP_FAILPOINT.with(|failpoint| {
        if failpoint.replace(false) {
            Err(CoreError::InvalidState(
                "provider bootstrap failpoint".to_owned(),
            ))
        } else {
            Ok(())
        }
    })
}

#[cfg(not(test))]
const fn provider_bootstrap_failpoint() -> Result<(), CoreError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{PROVIDER_BOOTSTRAP_FAILPOINT, persist_private_noclobber};

    #[test]
    fn provider_bootstrap_resumes_after_staging_crash_and_is_idempotent() {
        let directory = tempfile::TempDir::new().expect("directory");
        let path = directory.path().join("provider-connections.json");
        let expected = br#"{"schemaVersion":1,"providers":{}}"#;
        PROVIDER_BOOTSTRAP_FAILPOINT.with(|failpoint| failpoint.set(true));
        assert!(persist_private_noclobber(&path, expected).is_err());
        assert!(
            !path.exists(),
            "a pre-publish crash leaves no partial target"
        );

        persist_private_noclobber(&path, expected).expect("restart publishes state");
        persist_private_noclobber(&path, expected).expect("repeated restart is idempotent");
        assert_eq!(std::fs::read(&path).expect("published state"), expected);
        assert!(persist_private_noclobber(&path, b"different").is_err());
        assert_eq!(std::fs::read(path).expect("incumbent preserved"), expected);
    }
}
