//! Validated identities, peers, and folder capabilities for rclone sessions.

use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};
use uuid::Uuid;

const MAX_FOLDER_MEMBERS: usize = 129;
const MAX_NAME_BYTES: usize = 128;
const MAX_LABEL_BYTES: usize = 256;
const MAX_PATH_BYTES: usize = 4096;
const MAX_CERTIFICATE_DER_BYTES: usize = 16 * 1024;
const DEVICE_ID_CHUNKS: usize = 8;
const DEVICE_ID_CHUNK_BYTES: usize = 7;
const DEVICE_ID_UNCHUNKED_BYTES: usize = DEVICE_ID_CHUNKS * DEVICE_ID_CHUNK_BYTES;
const LUHN_DATA_BYTES: usize = 13;
const LUHN_BLOCK_BYTES: usize = 14;
const LUHN_ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineConfigError {
    InvalidDeviceId,
    InvalidCertificate,
    InvalidName,
    InvalidAddress,
    InvalidFolder,
    UnsafeFolderRoot,
    DuplicateMember,
    LimitExceeded,
}

impl fmt::Display for EngineConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidDeviceId => "invalid engine device identity",
            Self::InvalidCertificate => "invalid engine identity certificate",
            Self::InvalidName => "invalid engine display name",
            Self::InvalidAddress => "invalid direct engine address",
            Self::InvalidFolder => "invalid engine folder",
            Self::UnsafeFolderRoot => "engine folder root is not safe",
            Self::DuplicateMember => "engine folder member is duplicated",
            Self::LimitExceeded => "engine configuration exceeded its limit",
        })
    }
}

impl std::error::Error for EngineConfigError {}

#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EngineDeviceId(Box<str>);

impl fmt::Debug for EngineDeviceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("EngineDeviceId")
            .field(&self.0)
            .finish()
    }
}

impl EngineDeviceId {
    pub fn parse(value: &str) -> Result<Self, EngineConfigError> {
        let canonical = covalent_protocol::SyncEngineDeviceId::parse(value)
            .map_err(|_| EngineConfigError::InvalidDeviceId)?;
        Ok(Self(canonical.as_str().into()))
    }

    pub fn from_certificate_der(der: &[u8]) -> Result<Self, EngineConfigError> {
        if der.is_empty() || der.len() > MAX_CERTIFICATE_DER_BYTES {
            return Err(EngineConfigError::InvalidCertificate);
        }
        let digest = Sha256::digest(der);
        let mut payload = [0_u8; 52];
        let mut accumulator = 0_u32;
        let mut bits = 0_u8;
        let mut position = 0_usize;
        for byte in digest {
            accumulator = (accumulator << 8) | u32::from(byte);
            bits += 8;
            while bits >= 5 {
                bits -= 5;
                payload[position] = LUHN_ALPHABET[((accumulator >> bits) & 31) as usize];
                position += 1;
            }
            accumulator &= (1_u32 << bits) - 1;
        }
        if bits > 0 {
            payload[position] = LUHN_ALPHABET[((accumulator << (5 - bits)) & 31) as usize];
            position += 1;
        }
        if position != payload.len() {
            return Err(EngineConfigError::InvalidCertificate);
        }
        let mut checked = [0_u8; DEVICE_ID_UNCHUNKED_BYTES];
        for (index, block) in payload.chunks_exact(LUHN_DATA_BYTES).enumerate() {
            let start = index * LUHN_BLOCK_BYTES;
            checked[start..start + LUHN_DATA_BYTES].copy_from_slice(block);
            checked[start + LUHN_DATA_BYTES] =
                luhn32(block).ok_or(EngineConfigError::InvalidCertificate)?;
        }
        let mut canonical = String::new();
        canonical
            .try_reserve(DEVICE_ID_UNCHUNKED_BYTES + DEVICE_ID_CHUNKS - 1)
            .map_err(|_| EngineConfigError::LimitExceeded)?;
        for (index, block) in checked.chunks_exact(DEVICE_ID_CHUNK_BYTES).enumerate() {
            if index > 0 {
                canonical.push('-');
            }
            for byte in block {
                canonical.push(char::from(*byte));
            }
        }
        Self::parse(&canonical)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn luhn32(value: &[u8]) -> Option<u8> {
    let mut factor = 1_usize;
    let mut sum = 0_usize;
    for byte in value {
        let codepoint = LUHN_ALPHABET
            .iter()
            .position(|candidate| candidate == byte)?;
        let addend = factor * codepoint;
        factor = if factor == 2 { 1 } else { 2 };
        sum += addend / LUHN_ALPHABET.len() + addend % LUHN_ALPHABET.len();
    }
    let check = (LUHN_ALPHABET.len() - sum % LUHN_ALPHABET.len()) % LUHN_ALPHABET.len();
    Some(LUHN_ALPHABET[check])
}

#[derive(Clone, Eq, PartialEq)]
pub struct EnginePeerConfig {
    id: EngineDeviceId,
    name: Box<str>,
    address: SocketAddr,
    ssh_public_key: Option<Box<str>>,
    paused: bool,
}

impl fmt::Debug for EnginePeerConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EnginePeerConfig")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("address", &self.address)
            .field("paused", &self.paused)
            .finish()
    }
}

impl EnginePeerConfig {
    pub fn new(
        id: EngineDeviceId,
        name: &str,
        address: SocketAddr,
    ) -> Result<Self, EngineConfigError> {
        validate_text(name, MAX_NAME_BYTES)?;
        validate_peer_address(address)?;
        Ok(Self {
            id,
            name: name.into(),
            address,
            ssh_public_key: None,
            paused: false,
        })
    }

    #[must_use]
    pub const fn with_paused(mut self, paused: bool) -> Self {
        self.paused = paused;
        self
    }

    pub fn with_ssh_public_key(mut self, value: &str) -> Result<Self, EngineConfigError> {
        covalent_protocol::SyncEngineBinding::new_authenticated(
            covalent_protocol::DeviceId::from_uuid(Uuid::new_v4()),
            self.id.as_str(),
            value,
            &self.address.to_string(),
        )
        .map_err(|_| EngineConfigError::InvalidDeviceId)?;
        self.ssh_public_key = Some(value.into());
        Ok(self)
    }

    pub fn id(&self) -> &EngineDeviceId {
        &self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn address(&self) -> SocketAddr {
        self.address
    }

    pub fn ssh_public_key(&self) -> Option<&str> {
        self.ssh_public_key.as_deref()
    }

    pub const fn paused(&self) -> bool {
        self.paused
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum EngineFolderRole {
    #[default]
    LegacyTwoWay,
    Source,
    Destination {
        propagate_source_deletions: bool,
        restore_local_deletions: bool,
    },
}

#[derive(Clone, Eq, PartialEq)]
pub struct EngineFolderConfig {
    id: Uuid,
    label: Box<str>,
    root: PathBuf,
    members: Vec<EngineDeviceId>,
    paused: bool,
    role: EngineFolderRole,
}

impl fmt::Debug for EngineFolderConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EngineFolderConfig")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("root", &"[PRIVATE]")
            .field("members", &self.members)
            .field("paused", &self.paused)
            .field("role", &self.role)
            .finish()
    }
}

impl EngineFolderConfig {
    pub fn new(
        id: Uuid,
        label: &str,
        root: PathBuf,
        mut members: Vec<EngineDeviceId>,
    ) -> Result<Self, EngineConfigError> {
        if id.is_nil() {
            return Err(EngineConfigError::InvalidFolder);
        }
        validate_text(label, MAX_LABEL_BYTES)?;
        if members.len() > MAX_FOLDER_MEMBERS {
            return Err(EngineConfigError::LimitExceeded);
        }
        members.sort();
        if members.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(EngineConfigError::DuplicateMember);
        }
        Ok(Self {
            id,
            label: label.into(),
            root: validate_root(&root)?,
            members,
            paused: false,
            role: EngineFolderRole::default(),
        })
    }

    #[must_use]
    pub const fn with_role(mut self, role: EngineFolderRole) -> Self {
        self.role = role;
        self
    }

    #[must_use]
    pub const fn with_paused(mut self, paused: bool) -> Self {
        self.paused = paused;
        self
    }

    pub const fn role(&self) -> EngineFolderRole {
        self.role
    }

    pub const fn id(&self) -> Uuid {
        self.id
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn members(&self) -> &[EngineDeviceId] {
        &self.members
    }

    pub const fn paused(&self) -> bool {
        self.paused
    }
}

fn validate_text(value: &str, maximum: usize) -> Result<(), EngineConfigError> {
    if value.is_empty()
        || value.len() > maximum
        || value
            .chars()
            .any(|character| character.is_control() || matches!(character, '\u{fffe}' | '\u{ffff}'))
    {
        return Err(EngineConfigError::InvalidName);
    }
    Ok(())
}

fn validate_root(path: &Path) -> Result<PathBuf, EngineConfigError> {
    if super::android_saf::parse_token(path)
        .map_err(|_| EngineConfigError::InvalidFolder)?
        .is_some()
    {
        return Ok(path.to_path_buf());
    }
    let bytes = path.as_os_str().as_bytes();
    let Some(text) = path.to_str() else {
        return Err(EngineConfigError::UnsafeFolderRoot);
    };
    if !path.is_absolute()
        || bytes.is_empty()
        || bytes.len() > MAX_PATH_BYTES
        || bytes.contains(&0)
        || has_unsafe_path_text(text)
    {
        return Err(EngineConfigError::UnsafeFolderRoot);
    }
    let canonical = std::fs::canonicalize(path).map_err(|_| EngineConfigError::UnsafeFolderRoot)?;
    let metadata =
        std::fs::symlink_metadata(&canonical).map_err(|_| EngineConfigError::UnsafeFolderRoot)?;
    let Some(canonical_text) = canonical.to_str() else {
        return Err(EngineConfigError::UnsafeFolderRoot);
    };
    if !metadata.is_dir()
        || canonical.as_os_str().as_bytes().len() > MAX_PATH_BYTES
        || has_unsafe_path_text(canonical_text)
        || canonical.parent().is_none()
    {
        return Err(EngineConfigError::UnsafeFolderRoot);
    }
    Ok(canonical)
}

fn has_unsafe_path_text(value: &str) -> bool {
    value
        .chars()
        .any(|character| character.is_control() || matches!(character, '\u{fffe}' | '\u{ffff}'))
}

pub(super) fn validate_peer_address(address: SocketAddr) -> Result<(), EngineConfigError> {
    if address.port() == 0 || unacceptable_ip(address.ip()) {
        return Err(EngineConfigError::InvalidAddress);
    }
    Ok(())
}

fn unacceptable_ip(address: IpAddr) -> bool {
    address.is_unspecified() || address.is_multicast()
}
