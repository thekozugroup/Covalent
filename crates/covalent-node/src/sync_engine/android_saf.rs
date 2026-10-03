//! Runtime-only Android SAF capabilities.

use std::collections::BTreeMap;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use uuid::Uuid;
use zeroize::Zeroizing;

const TOKEN_PREFIX: &str = "covalent-saf:";
const MAX_USERNAME_BYTES: usize = 128;
const MAX_PASSWORD_BYTES: usize = 512;

/// A rejected runtime-only SAF registration. Errors contain no credentials.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AndroidSafGrantError {
    InvalidGrantId,
    InvalidPort,
    InvalidCredential,
    Busy,
    Unavailable,
}

impl fmt::Display for AndroidSafGrantError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidGrantId => "folder grant identifier is invalid",
            Self::InvalidPort => "folder grant endpoint is invalid",
            Self::InvalidCredential => "folder grant credential is invalid",
            Self::Busy => "folder grant is in use",
            Self::Unavailable => "folder grant registry is unavailable",
        })
    }
}

impl std::error::Error for AndroidSafGrantError {}

#[derive(Clone)]
pub(super) struct AndroidSafGrant {
    pub(super) address: SocketAddr,
    pub(super) username: Zeroizing<String>,
    pub(super) password: Zeroizing<String>,
}

impl fmt::Debug for AndroidSafGrant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AndroidSafGrant([PRIVATE])")
    }
}

#[derive(Default)]
struct RegistryState {
    active: BTreeMap<Uuid, usize>,
    grants: BTreeMap<Uuid, AndroidSafGrant>,
}

struct RegistryInner {
    state: Mutex<RegistryState>,
    obscurer: Option<Arc<dyn PasswordObscurer>>,
}

pub(super) trait PasswordObscurer: Send + Sync {
    fn obscure(
        &self,
        password: Zeroizing<String>,
    ) -> Result<Zeroizing<String>, AndroidSafGrantError>;
}

/// App-owned loopback WebDAV grants registered directly by the Android host.
///
/// The public folder API stores only [`Self::token`]. URLs and credentials stay
/// in this process and never enter share records or status responses.
#[derive(Clone)]
pub struct AndroidSafGrantRegistry {
    inner: Arc<RegistryInner>,
}

impl Default for AndroidSafGrantRegistry {
    fn default() -> Self {
        Self {
            inner: Arc::new(RegistryInner {
                state: Mutex::new(RegistryState::default()),
                obscurer: None,
            }),
        }
    }
}

impl fmt::Debug for AndroidSafGrantRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AndroidSafGrantRegistry([PRIVATE])")
    }
}

impl AndroidSafGrantRegistry {
    pub(super) fn with_obscurer(obscurer: Arc<dyn PasswordObscurer>) -> Self {
        Self {
            inner: Arc::new(RegistryInner {
                state: Mutex::new(RegistryState::default()),
                obscurer: Some(obscurer),
            }),
        }
    }

    /// Return the only token form accepted by the existing `selectedRoot` API.
    #[must_use]
    pub fn token(grant_id: Uuid) -> PathBuf {
        PathBuf::from(format!("{TOKEN_PREFIX}{grant_id}"))
    }

    /// Register one authenticated app-owned IPv4 loopback WebDAV capability.
    pub fn register(
        &self,
        grant_id: Uuid,
        address: SocketAddr,
        username: Zeroizing<String>,
        password: Zeroizing<String>,
    ) -> Result<(), AndroidSafGrantError> {
        if grant_id.is_nil() {
            return Err(AndroidSafGrantError::InvalidGrantId);
        }
        if address.ip() != IpAddr::V4(Ipv4Addr::LOCALHOST) || address.port() == 0 {
            return Err(AndroidSafGrantError::InvalidPort);
        }
        if !valid_username(&username) || !valid_password(&password) {
            return Err(AndroidSafGrantError::InvalidCredential);
        }
        let password = self
            .inner
            .obscurer
            .as_ref()
            .ok_or(AndroidSafGrantError::Unavailable)?
            .obscure(password)?;
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| AndroidSafGrantError::Unavailable)?;
        if state.active.get(&grant_id).copied().unwrap_or(0) != 0 {
            return Err(AndroidSafGrantError::Busy);
        }
        if state.grants.get(&grant_id).is_some_and(|current| {
            current.address == address
                && current.username.as_str() == username.as_str()
                && current.password.as_str() == password.as_str()
        }) {
            return Ok(());
        }
        state.grants.insert(
            grant_id,
            AndroidSafGrant {
                address,
                username,
                password,
            },
        );
        Ok(())
    }

    /// Remove an inactive capability. Active transfers keep their exact grant.
    pub fn unregister(&self, grant_id: Uuid) -> Result<(), AndroidSafGrantError> {
        if grant_id.is_nil() {
            return Err(AndroidSafGrantError::InvalidGrantId);
        }
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| AndroidSafGrantError::Unavailable)?;
        if state.active.get(&grant_id).copied().unwrap_or(0) != 0 {
            return Err(AndroidSafGrantError::Busy);
        }
        state.grants.remove(&grant_id);
        Ok(())
    }

    pub(super) fn resolve(
        &self,
        selected_root: &Path,
    ) -> Result<Option<(Uuid, AndroidSafGrant)>, AndroidSafGrantError> {
        let Some(grant_id) = parse_token(selected_root)? else {
            return Ok(None);
        };
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| AndroidSafGrantError::Unavailable)?;
        Ok(state
            .grants
            .get(&grant_id)
            .cloned()
            .map(|grant| (grant_id, grant)))
    }

    pub(super) fn contains_token(
        &self,
        selected_root: &Path,
    ) -> Result<bool, AndroidSafGrantError> {
        let Some(grant_id) = parse_token(selected_root)? else {
            return Ok(false);
        };
        self.inner
            .state
            .lock()
            .map(|state| state.grants.contains_key(&grant_id))
            .map_err(|_| AndroidSafGrantError::Unavailable)
    }

    pub(super) fn mark_active(&self, grant_id: Uuid) -> Result<(), AndroidSafGrantError> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| AndroidSafGrantError::Unavailable)?;
        if !state.grants.contains_key(&grant_id) {
            return Err(AndroidSafGrantError::Unavailable);
        }
        *state.active.entry(grant_id).or_default() += 1;
        Ok(())
    }

    pub(super) fn mark_inactive(&self, grant_id: Uuid) {
        let Ok(mut state) = self.inner.state.lock() else {
            return;
        };
        let Some(count) = state.active.get_mut(&grant_id) else {
            return;
        };
        *count -= 1;
        if *count == 0 {
            state.active.remove(&grant_id);
        }
    }
}

pub(super) fn parse_token(selected_root: &Path) -> Result<Option<Uuid>, AndroidSafGrantError> {
    let Some(value) = selected_root.to_str() else {
        return Ok(None);
    };
    let Some(value) = value.strip_prefix(TOKEN_PREFIX) else {
        return Ok(None);
    };
    let grant_id = Uuid::parse_str(value).map_err(|_| AndroidSafGrantError::InvalidGrantId)?;
    if grant_id.is_nil() || value != grant_id.to_string() {
        return Err(AndroidSafGrantError::InvalidGrantId);
    }
    Ok(Some(grant_id))
}

fn valid_username(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_USERNAME_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'~' | b'-'))
}

fn valid_password(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_PASSWORD_BYTES
        && !value
            .bytes()
            .any(|byte| byte == 0 || byte == b'\n' || byte == b'\r')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret(value: &str) -> Zeroizing<String> {
        Zeroizing::new(value.to_owned())
    }

    struct TestObscurer;

    impl PasswordObscurer for TestObscurer {
        fn obscure(
            &self,
            password: Zeroizing<String>,
        ) -> Result<Zeroizing<String>, AndroidSafGrantError> {
            Ok(password)
        }
    }

    fn registry() -> AndroidSafGrantRegistry {
        AndroidSafGrantRegistry::with_obscurer(Arc::new(TestObscurer))
    }

    #[test]
    fn registration_accepts_only_exact_loopback_capabilities() {
        let registry = registry();
        let id = Uuid::new_v4();
        let address = "127.0.0.1:48123".parse().unwrap();
        registry
            .register(id, address, secret("grant_user"), secret("secret-value"))
            .unwrap();
        assert!(
            registry
                .contains_token(&AndroidSafGrantRegistry::token(id))
                .unwrap()
        );
        let resolved = registry
            .resolve(&AndroidSafGrantRegistry::token(id))
            .unwrap()
            .unwrap();
        assert_eq!(resolved.0, id);
        assert_eq!(resolved.1.address, address);
        assert!(
            registry
                .register(
                    Uuid::new_v4(),
                    "192.0.2.1:48123".parse().unwrap(),
                    secret("user"),
                    secret("secret"),
                )
                .is_err()
        );
        assert!(
            registry
                .contains_token(Path::new("/tmp/local"))
                .is_ok_and(|v| !v)
        );
        assert!(
            registry
                .resolve(Path::new("covalent-saf:not-a-uuid"))
                .is_err()
        );
    }

    #[test]
    fn active_grant_cannot_change_or_unregister() {
        let registry = registry();
        let id = Uuid::new_v4();
        let address = "127.0.0.1:48123".parse().unwrap();
        registry
            .register(id, address, secret("user"), secret("secret"))
            .unwrap();
        registry.mark_active(id).unwrap();
        assert_eq!(registry.unregister(id), Err(AndroidSafGrantError::Busy));
        assert_eq!(
            registry.register(id, address, secret("user"), secret("changed")),
            Err(AndroidSafGrantError::Busy)
        );
        registry.mark_inactive(id);
        registry.unregister(id).unwrap();
        assert!(
            !registry
                .contains_token(&AndroidSafGrantRegistry::token(id))
                .unwrap()
        );
    }
}
