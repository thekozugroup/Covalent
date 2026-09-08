//! Validated, deterministic configuration for the pinned folder-sync engine.
//!
//! This module renders a complete configuration before the engine opens any
//! network listener. It does not parse engine-owned XML and does not manage the
//! engine identity key or database. The controller must keep those in separate
//! private storage and verify the effective configuration after startup.

use std::collections::BTreeSet;
use std::fmt::{self, Write as _};
use std::net::{IpAddr, SocketAddr};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;
use sha2::{Digest as _, Sha256};
use uuid::Uuid;
use zeroize::Zeroizing;

const PINNED_CONFIG_VERSION: u8 = 52;
const MAX_PEERS: usize = 128;
const MAX_FOLDERS: usize = 128;
const MAX_FOLDER_MEMBERS: usize = MAX_PEERS + 1;
const MAX_TOTAL_FOLDER_MEMBERS: usize = 4096;
const MAX_NAME_BYTES: usize = 128;
const MAX_LABEL_BYTES: usize = 256;
const MAX_PATH_BYTES: usize = 4096;
const MAX_GUI_SOCKET_BYTES: usize = 100;
const MAX_RENDERED_XML_BYTES: usize = 4 * 1024 * 1024;
const API_KEY_BYTES: usize = 64;
const MAX_CERTIFICATE_DER_BYTES: usize = 16 * 1024;
const DEVICE_ID_CHUNKS: usize = 8;
const DEVICE_ID_CHUNK_BYTES: usize = 7;
const DEVICE_ID_UNCHUNKED_BYTES: usize = DEVICE_ID_CHUNKS * DEVICE_ID_CHUNK_BYTES;
const LUHN_DATA_BYTES: usize = 13;
const LUHN_BLOCK_BYTES: usize = 14;
const LUHN_ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// Fixed configuration failures. Values that may contain private paths or
/// credentials are deliberately absent from every variant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineConfigError {
    /// An engine identity was not exact canonical Base32 with valid checks.
    InvalidDeviceId,
    /// Certificate bytes were empty or exceeded their fixed input bound.
    InvalidCertificate,
    /// The private API key was not exactly 64 hexadecimal characters.
    InvalidApiKey,
    /// A device name or folder label was empty, unsafe or too long.
    InvalidName,
    /// A sync listener or peer address was unusable.
    InvalidAddress,
    /// The owner-only GUI socket path was unusable.
    InvalidSocketPath,
    /// A folder identifier or membership was unusable.
    InvalidFolder,
    /// A folder root was absent, non-directory or not canonically addressable.
    UnsafeFolderRoot,
    /// A remote engine identity appeared more than once.
    DuplicateDevice,
    /// A folder UUID appeared more than once.
    DuplicateFolder,
    /// A folder repeated one member identity.
    DuplicateMember,
    /// A folder named an identity outside the configured peers.
    UnknownMember,
    /// The local identity was incorrectly configured as a remote peer.
    SelfPeer,
    /// Two canonically resolved folder roots contain one another.
    OverlappingRoots,
    /// A folder lacked the local identity or any paired peer.
    MissingPeer,
    /// A shared folder was configured without a sync listener.
    MissingListener,
    /// An input or rendered configuration exceeded a fixed bound.
    LimitExceeded,
    /// A bounded configuration could not be encoded.
    RenderFailed,
    /// The running engine did not retain the exact controlled configuration.
    EffectiveConfigMismatch,
}

impl fmt::Display for EngineConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidDeviceId => "invalid engine device identity",
            Self::InvalidCertificate => "invalid engine identity certificate",
            Self::InvalidApiKey => "invalid private engine credential",
            Self::InvalidName => "invalid engine display name",
            Self::InvalidAddress => "invalid direct engine address",
            Self::InvalidSocketPath => "invalid private engine socket path",
            Self::InvalidFolder => "invalid engine folder",
            Self::UnsafeFolderRoot => "engine folder root is not safe",
            Self::DuplicateDevice => "engine device is duplicated",
            Self::DuplicateFolder => "engine folder is duplicated",
            Self::DuplicateMember => "engine folder member is duplicated",
            Self::UnknownMember => "engine folder contains an unknown member",
            Self::SelfPeer => "engine peer is the local device",
            Self::OverlappingRoots => "engine folder roots overlap",
            Self::MissingPeer => "shared engine folder requires a peer",
            Self::MissingListener => "shared engine folder requires a listener",
            Self::LimitExceeded => "engine configuration exceeded its limit",
            Self::RenderFailed => "engine configuration could not be rendered",
            Self::EffectiveConfigMismatch => "effective engine configuration does not match",
        })
    }
}

impl std::error::Error for EngineConfigError {}

/// Canonical Syncthing device identity, including all four Luhn-32 check
/// digits. User-friendly lowercase, spaces and typo substitutions are rejected;
/// the authenticated pairing layer must persist one exact canonical value.
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
    /// Validate and retain one exact canonical upstream identity string.
    pub fn parse(value: &str) -> Result<Self, EngineConfigError> {
        let canonical = covalent_protocol::SyncEngineDeviceId::parse(value)
            .map_err(|_| EngineConfigError::InvalidDeviceId)?;
        Ok(Self(canonical.as_str().into()))
    }

    /// Derive the upstream identity from certificate DER using SHA-256 and the
    /// canonical Base32/check-digit representation. This only applies the
    /// upstream identity transform; the caller must separately validate that
    /// the DER is the expected pinned certificate/key type and binding.
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

    /// Return the canonical public identity.
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

/// Controller-generated 256-bit hexadecimal REST credential.
pub struct EngineApiKey(Zeroizing<String>);

impl fmt::Debug for EngineApiKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("EngineApiKey([PRIVATE])")
    }
}

impl EngineApiKey {
    /// Validate and retain a 64-character hexadecimal key.
    pub fn parse(value: Zeroizing<String>) -> Result<Self, EngineConfigError> {
        if value.len() != API_KEY_BYTES || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(EngineConfigError::InvalidApiKey);
        }
        Ok(Self(value))
    }

    /// Make the one explicit zeroizing copy needed when configuration and the
    /// private REST client have overlapping lifetimes.
    pub fn clone_secret(&self) -> Zeroizing<String> {
        Zeroizing::new(self.0.to_string())
    }

    fn expose(&self) -> &str {
        self.0.as_str()
    }
}

/// One explicitly paired remote engine and its only direct address.
#[derive(Clone, Eq, PartialEq)]
pub struct EnginePeerConfig {
    id: EngineDeviceId,
    name: Box<str>,
    address: SocketAddr,
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
    /// Validate one paired peer with one explicit direct TCP address.
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
            paused: false,
        })
    }

    /// Select whether this peer is present but administratively paused.
    #[must_use]
    pub const fn with_paused(mut self, paused: bool) -> Self {
        self.paused = paused;
        self
    }

    /// Return the peer's canonical public identity.
    pub fn id(&self) -> &EngineDeviceId {
        &self.id
    }

    /// Return the bounded display name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Return the direct peer address.
    pub const fn address(&self) -> SocketAddr {
        self.address
    }

    /// Return whether this peer is administratively paused.
    pub const fn paused(&self) -> bool {
        self.paused
    }
}

/// One exact local root and the explicit engine identities authorized to share
/// it. Root admission here is provisional; the controller must retain and
/// revalidate its no-follow descriptor capability before starting the engine.
#[derive(Clone, Eq, PartialEq)]
pub struct EngineFolderConfig {
    id: Uuid,
    label: Box<str>,
    root: PathBuf,
    members: Vec<EngineDeviceId>,
    paused: bool,
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
            .finish()
    }
}

impl EngineFolderConfig {
    /// Admit an existing canonical local directory and explicit members.
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
        let root = validate_root(&root)?;
        Ok(Self {
            id,
            label: label.into(),
            root,
            members,
            paused: false,
        })
    }

    /// Select whether this folder is present but administratively paused.
    #[must_use]
    pub const fn with_paused(mut self, paused: bool) -> Self {
        self.paused = paused;
        self
    }

    /// Return the folder UUID used as the engine folder ID.
    pub const fn id(&self) -> Uuid {
        self.id
    }

    /// Return the bounded user-visible label.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Return the canonical existing root. This is not a retained capability.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Return deterministically sorted explicit engine members.
    pub fn members(&self) -> &[EngineDeviceId] {
        &self.members
    }

    /// Return whether this folder is administratively paused.
    pub const fn paused(&self) -> bool {
        self.paused
    }
}

/// Fully validated desired state for one pinned engine process.
pub struct DesiredEngineConfig {
    own_id: EngineDeviceId,
    own_name: Box<str>,
    gui: EngineGuiEndpoint,
    api_key: EngineApiKey,
    listener: Option<SocketAddr>,
    peers: Vec<EnginePeerConfig>,
    folders: Vec<EngineFolderConfig>,
}

/// Private engine control transport. TLS is used when the native sandbox's
/// authorized temporary directory cannot fit a Unix-domain socket.
pub enum EngineGuiEndpoint {
    /// An owner-only Unix socket beneath the private runtime directory.
    Unix(PathBuf),
    /// Numeric loopback TLS with a separately pinned per-session certificate.
    LoopbackTls {
        /// Exact numeric loopback listener; no hostname or proxy resolution.
        address: SocketAddr,
        /// Private runtime directory, excluded from every shared folder.
        private_root: PathBuf,
    },
}

impl EngineGuiEndpoint {
    fn validate(self) -> Result<Self, EngineConfigError> {
        match self {
            Self::Unix(path) => Ok(Self::Unix(validate_socket_path(&path)?)),
            Self::LoopbackTls {
                address,
                private_root,
            } => {
                if !address.ip().is_loopback() || address.port() == 0 {
                    return Err(EngineConfigError::InvalidAddress);
                }
                if matches!(address, SocketAddr::V6(value) if value.scope_id() != 0 || value.flowinfo() != 0)
                {
                    return Err(EngineConfigError::InvalidAddress);
                }
                Ok(Self::LoopbackTls {
                    address,
                    private_root: validate_root(&private_root)?,
                })
            }
        }
    }

    fn private_root(&self) -> Result<&Path, EngineConfigError> {
        match self {
            Self::Unix(path) => path.parent().ok_or(EngineConfigError::InvalidSocketPath),
            Self::LoopbackTls { private_root, .. } => Ok(private_root),
        }
    }

    fn address(&self) -> Result<String, EngineConfigError> {
        match self {
            Self::Unix(path) => path
                .to_str()
                .map(str::to_owned)
                .ok_or(EngineConfigError::InvalidSocketPath),
            Self::LoopbackTls { address, .. } => Ok(address.to_string()),
        }
    }

    const fn uses_tls(&self) -> bool {
        matches!(self, Self::LoopbackTls { .. })
    }
}

impl fmt::Debug for DesiredEngineConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DesiredEngineConfig")
            .field("own_id", &self.own_id)
            .field("own_name", &self.own_name)
            .field("gui", &"[PRIVATE]")
            .field("api_key", &self.api_key)
            .field("listener", &self.listener)
            .field("peers", &self.peers)
            .field("folders", &self.folders)
            .finish()
    }
}

impl DesiredEngineConfig {
    /// Validate one complete desired engine configuration.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        own_id: EngineDeviceId,
        own_name: &str,
        gui_socket: PathBuf,
        api_key: EngineApiKey,
        listener: Option<SocketAddr>,
        peers: Vec<EnginePeerConfig>,
        folders: Vec<EngineFolderConfig>,
    ) -> Result<Self, EngineConfigError> {
        Self::with_control(
            own_id,
            own_name,
            EngineGuiEndpoint::Unix(gui_socket),
            api_key,
            listener,
            peers,
            folders,
        )
    }

    /// Validate a complete configuration with an explicit private transport.
    pub fn with_control(
        own_id: EngineDeviceId,
        own_name: &str,
        gui: EngineGuiEndpoint,
        api_key: EngineApiKey,
        listener: Option<SocketAddr>,
        mut peers: Vec<EnginePeerConfig>,
        mut folders: Vec<EngineFolderConfig>,
    ) -> Result<Self, EngineConfigError> {
        validate_text(own_name, MAX_NAME_BYTES)?;
        let gui = gui.validate()?;
        if peers.len() > MAX_PEERS || folders.len() > MAX_FOLDERS {
            return Err(EngineConfigError::LimitExceeded);
        }
        if let Some(address) = listener {
            validate_listener_address(address)?;
        }
        peers.sort_by(|left, right| left.id.cmp(&right.id));
        if peers.iter().any(|peer| peer.id == own_id) {
            return Err(EngineConfigError::SelfPeer);
        }
        if peers.windows(2).any(|pair| pair[0].id == pair[1].id) {
            return Err(EngineConfigError::DuplicateDevice);
        }
        folders.sort_by_key(|folder| folder.id);
        if folders.windows(2).any(|pair| pair[0].id == pair[1].id) {
            return Err(EngineConfigError::DuplicateFolder);
        }
        let total_members = folders.iter().try_fold(0_usize, |total, folder| {
            total.checked_add(folder.members.len())
        });
        if !matches!(total_members, Some(total) if total <= MAX_TOTAL_FOLDER_MEMBERS) {
            return Err(EngineConfigError::LimitExceeded);
        }
        let peer_ids = peers
            .iter()
            .map(|peer| peer.id.clone())
            .collect::<BTreeSet<_>>();
        for folder in &folders {
            if folder.members.len() < 2 || !folder.members.contains(&own_id) {
                return Err(EngineConfigError::MissingPeer);
            }
            for member in &folder.members {
                if member != &own_id && !peer_ids.contains(member) {
                    return Err(EngineConfigError::UnknownMember);
                }
            }
        }
        if !folders.is_empty() && listener.is_none() {
            return Err(EngineConfigError::MissingListener);
        }
        for (index, folder) in folders.iter().enumerate() {
            if folders[index + 1..]
                .iter()
                .any(|other| roots_overlap(&folder.root, &other.root))
            {
                return Err(EngineConfigError::OverlappingRoots);
            }
        }
        let gui_parent = gui.private_root()?;
        if folders
            .iter()
            .any(|folder| roots_overlap(&folder.root, gui_parent))
        {
            return Err(EngineConfigError::UnsafeFolderRoot);
        }
        Ok(Self {
            own_id,
            own_name: own_name.into(),
            gui,
            api_key,
            listener,
            peers,
            folders,
        })
    }

    /// Return the local canonical public identity.
    pub fn own_id(&self) -> &EngineDeviceId {
        &self.own_id
    }

    /// Return the local bounded display name.
    pub fn own_name(&self) -> &str {
        &self.own_name
    }

    /// Return the private Unix socket path, if this session uses Unix control.
    pub fn gui_socket(&self) -> Option<&Path> {
        match &self.gui {
            EngineGuiEndpoint::Unix(path) => Some(path),
            EngineGuiEndpoint::LoopbackTls { .. } => None,
        }
    }

    /// Return the explicit TCP listener, if sharing is enabled.
    pub const fn listener(&self) -> Option<SocketAddr> {
        self.listener
    }

    /// Return deterministically sorted paired peers.
    pub fn peers(&self) -> &[EnginePeerConfig] {
        &self.peers
    }

    /// Return deterministically sorted configured folders.
    pub fn folders(&self) -> &[EngineFolderConfig] {
        &self.folders
    }

    /// Make an explicit zeroizing API-key copy for the private REST client.
    pub fn api_key_copy(&self) -> Zeroizing<String> {
        self.api_key.clone_secret()
    }

    /// Render a network-inert first-start configuration. It retains the exact
    /// identity and private API but has no peer, folder or sync listener. A
    /// later desired-state replacement is required before sharing can begin.
    pub fn render_initial_xml(&self) -> Result<Zeroizing<String>, EngineConfigError> {
        self.render(RenderMode::Initial)
    }

    /// Render all authorized folders while keeping every remote device paused
    /// and every sync listener disabled. This lets the controller positively
    /// scan local roots before the worker can exchange indexes or content.
    pub fn render_scan_gate_xml(&self) -> Result<Zeroizing<String>, EngineConfigError> {
        self.render(RenderMode::ScanGate)
    }

    /// Render the complete desired configuration. Folders are active only when
    /// an explicit direct listener, paired peer and membership are all present.
    /// `ignorePerms=true` avoids propagating Unix permission bits across
    /// platforms. Callers must not infer executable-mode synchronization.
    pub fn render_desired_xml(&self) -> Result<Zeroizing<String>, EngineConfigError> {
        self.render(RenderMode::Desired)
    }

    /// Verify the effective configuration returned by the pinned engine after
    /// applying the complete desired state. Expanded unrelated defaults are
    /// allowed, while every Covalent-controlled field must match exactly.
    pub fn verify_effective(&self, effective: &Value) -> Result<(), EngineConfigError> {
        self.verify_effective_mode(effective, EffectiveMode::Desired)
    }

    /// Verify the effective configuration returned after the network-inert
    /// first-start configuration. Only the local identity and private API may
    /// be present; folders, peers, and sync listeners must remain absent.
    pub fn verify_initial_effective(&self, effective: &Value) -> Result<(), EngineConfigError> {
        self.verify_effective_mode(effective, EffectiveMode::Initial)
    }

    /// Verify the exact network-inert folder configuration used for the
    /// mandatory initial scan.
    pub fn verify_scan_gate_effective(&self, effective: &Value) -> Result<(), EngineConfigError> {
        self.verify_effective_mode(effective, EffectiveMode::ScanGate)
    }

    /// Derive the desired configuration payload only from a freshly fetched
    /// and exactly verified scan-gate configuration. The pinned worker accepts
    /// this complete JSON object through its private configuration endpoint.
    pub(super) fn promotion_payload(
        &self,
        mut effective: Value,
    ) -> Result<Value, EngineConfigError> {
        self.verify_scan_gate_effective(&effective)?;
        let options = effective
            .get_mut("options")
            .and_then(Value::as_object_mut)
            .ok_or(EngineConfigError::EffectiveConfigMismatch)?;
        let listener = self
            .listener
            .map(|address| format!("tcp://{address}"))
            .unwrap_or_default();
        options.insert("listenAddresses".to_owned(), serde_json::json!([listener]));
        let devices = effective
            .get_mut("devices")
            .and_then(Value::as_array_mut)
            .ok_or(EngineConfigError::EffectiveConfigMismatch)?;
        for peer in &self.peers {
            let device = devices
                .iter_mut()
                .find(|candidate| {
                    candidate.get("deviceID").and_then(Value::as_str) == Some(peer.id.as_str())
                })
                .and_then(Value::as_object_mut)
                .ok_or(EngineConfigError::EffectiveConfigMismatch)?;
            device.insert("paused".to_owned(), Value::Bool(peer.paused));
        }
        let folders = effective
            .get_mut("folders")
            .and_then(Value::as_array_mut)
            .ok_or(EngineConfigError::EffectiveConfigMismatch)?;
        for folder in &self.folders {
            let id = folder.id.to_string();
            let effective_folder = folders
                .iter_mut()
                .find(|candidate| candidate.get("id").and_then(Value::as_str) == Some(&id))
                .and_then(Value::as_object_mut)
                .ok_or(EngineConfigError::EffectiveConfigMismatch)?;
            effective_folder.insert("paused".to_owned(), Value::Bool(folder.paused));
        }
        self.verify_effective(&effective)?;
        Ok(effective)
    }

    fn verify_effective_mode(
        &self,
        effective: &Value,
        mode: EffectiveMode,
    ) -> Result<(), EngineConfigError> {
        verify_gui(effective_field(effective, "gui")?, self)?;
        verify_options(effective_field(effective, "options")?, self, mode)?;
        verify_devices(effective_field(effective, "devices")?, self, mode)?;
        verify_folders(effective_field(effective, "folders")?, self, mode)
    }

    fn render(&self, mode: RenderMode) -> Result<Zeroizing<String>, EngineConfigError> {
        let mut xml = XmlWriter::new();
        writeln!(xml, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>")
            .map_err(|_| EngineConfigError::RenderFailed)?;
        writeln!(xml, "<configuration version=\"{PINNED_CONFIG_VERSION}\">")
            .map_err(|_| EngineConfigError::RenderFailed)?;
        if mode.includes_shares() {
            for folder in &self.folders {
                write_folder(
                    &mut xml,
                    folder,
                    mode == RenderMode::Desired && folder.paused,
                )?;
            }
        }
        write_device(&mut xml, &self.own_id, &self.own_name, None, false)?;
        if mode.includes_shares() {
            for peer in &self.peers {
                write_device(
                    &mut xml,
                    &peer.id,
                    &peer.name,
                    Some(peer.address),
                    mode == RenderMode::ScanGate || peer.paused,
                )?;
            }
        }
        write_gui(&mut xml, &self.gui, &self.api_key)?;
        xml.push("  <ldap></ldap>\n")?;
        write_options(
            &mut xml,
            (mode == RenderMode::Desired)
                .then_some(self.listener)
                .flatten(),
        )?;
        xml.push("  <defaults></defaults>\n")?;
        xml.push("</configuration>\n")?;
        Ok(Zeroizing::new(xml.finish()))
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

fn validate_socket_path(path: &Path) -> Result<PathBuf, EngineConfigError> {
    let bytes = path.as_os_str().as_bytes();
    let Some(text) = path.to_str() else {
        return Err(EngineConfigError::InvalidSocketPath);
    };
    if !path.is_absolute()
        || bytes.is_empty()
        || bytes.len() > MAX_GUI_SOCKET_BYTES
        || bytes.contains(&0)
        || has_unsafe_path_text(text)
        || path.file_name().is_none()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::CurDir | Component::Prefix(_)
            )
        })
    {
        return Err(EngineConfigError::InvalidSocketPath);
    }
    let parent = path.parent().ok_or(EngineConfigError::InvalidSocketPath)?;
    let canonical_parent =
        std::fs::canonicalize(parent).map_err(|_| EngineConfigError::InvalidSocketPath)?;
    let metadata = std::fs::symlink_metadata(&canonical_parent)
        .map_err(|_| EngineConfigError::InvalidSocketPath)?;
    let file_name = path
        .file_name()
        .ok_or(EngineConfigError::InvalidSocketPath)?;
    let canonical = canonical_parent.join(file_name);
    let Some(canonical_text) = canonical.to_str() else {
        return Err(EngineConfigError::InvalidSocketPath);
    };
    if !metadata.is_dir()
        || canonical.as_os_str().as_bytes().len() > MAX_GUI_SOCKET_BYTES
        || has_unsafe_path_text(canonical_text)
    {
        return Err(EngineConfigError::InvalidSocketPath);
    }
    Ok(canonical)
}

fn validate_root(path: &Path) -> Result<PathBuf, EngineConfigError> {
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

#[derive(Clone, Copy, Eq, PartialEq)]
enum RenderMode {
    Initial,
    ScanGate,
    Desired,
}

impl RenderMode {
    const fn includes_shares(self) -> bool {
        !matches!(self, Self::Initial)
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum EffectiveMode {
    Initial,
    ScanGate,
    Desired,
}

fn effective_mismatch<T>() -> Result<T, EngineConfigError> {
    Err(EngineConfigError::EffectiveConfigMismatch)
}

fn effective_field<'a>(value: &'a Value, name: &str) -> Result<&'a Value, EngineConfigError> {
    value
        .get(name)
        .ok_or(EngineConfigError::EffectiveConfigMismatch)
}

fn effective_string<'a>(value: &'a Value, name: &str) -> Result<&'a str, EngineConfigError> {
    effective_field(value, name)?
        .as_str()
        .ok_or(EngineConfigError::EffectiveConfigMismatch)
}

fn effective_bool(value: &Value, name: &str) -> Result<bool, EngineConfigError> {
    effective_field(value, name)?
        .as_bool()
        .ok_or(EngineConfigError::EffectiveConfigMismatch)
}

fn effective_i64(value: &Value, name: &str) -> Result<i64, EngineConfigError> {
    effective_field(value, name)?
        .as_i64()
        .ok_or(EngineConfigError::EffectiveConfigMismatch)
}

fn effective_array<'a>(value: &'a Value, name: &str) -> Result<&'a [Value], EngineConfigError> {
    effective_field(value, name)?
        .as_array()
        .map(Vec::as_slice)
        .ok_or(EngineConfigError::EffectiveConfigMismatch)
}

fn require_string(value: &Value, name: &str, expected: &str) -> Result<(), EngineConfigError> {
    if effective_string(value, name)? != expected {
        return effective_mismatch();
    }
    Ok(())
}

fn require_bool(value: &Value, name: &str, expected: bool) -> Result<(), EngineConfigError> {
    if effective_bool(value, name)? != expected {
        return effective_mismatch();
    }
    Ok(())
}

fn require_i64(value: &Value, name: &str, expected: i64) -> Result<(), EngineConfigError> {
    if effective_i64(value, name)? != expected {
        return effective_mismatch();
    }
    Ok(())
}

fn require_string_array(
    value: &Value,
    name: &str,
    expected: &[&str],
) -> Result<(), EngineConfigError> {
    let actual = effective_array(value, name)?;
    if actual.len() != expected.len()
        || actual
            .iter()
            .zip(expected)
            .any(|(actual, expected)| actual.as_str() != Some(expected))
    {
        return effective_mismatch();
    }
    Ok(())
}

fn verify_gui(value: &Value, expected: &DesiredEngineConfig) -> Result<(), EngineConfigError> {
    require_bool(value, "enabled", true)?;
    require_bool(value, "useTLS", expected.gui.uses_tls())?;
    require_bool(value, "sendBasicAuthPrompt", false)?;
    require_string(value, "address", &expected.gui.address()?)?;
    require_string(value, "unixSocketPermissions", "0600")?;
    require_string(value, "apiKey", expected.api_key.expose())?;
    require_bool(value, "metricsWithoutAuth", false)?;
    require_bool(value, "insecureAdminAccess", false)?;
    require_bool(value, "insecureSkipHostcheck", false)?;
    require_bool(value, "insecureAllowFrameLoading", false)
}

fn verify_options(
    value: &Value,
    expected: &DesiredEngineConfig,
    mode: EffectiveMode,
) -> Result<(), EngineConfigError> {
    let listener = match mode {
        EffectiveMode::Initial | EffectiveMode::ScanGate => String::new(),
        EffectiveMode::Desired => expected
            .listener
            .map(|address| format!("tcp://{address}"))
            .unwrap_or_default(),
    };
    require_string_array(value, "listenAddresses", &[&listener])?;
    require_string_array(value, "globalAnnounceServers", &[""])?;
    require_bool(value, "globalAnnounceEnabled", false)?;
    require_bool(value, "localAnnounceEnabled", false)?;
    require_bool(value, "relaysEnabled", false)?;
    require_bool(value, "natEnabled", false)?;
    require_bool(value, "startBrowser", false)?;
    require_i64(value, "urAccepted", -1)?;
    require_i64(value, "urSeen", -1)?;
    require_i64(value, "autoUpgradeIntervalH", 0)?;
    require_bool(value, "upgradeToPreReleases", false)?;
    require_bool(value, "crashReportingEnabled", false)?;
    require_bool(value, "announceLANAddresses", false)?;
    require_bool(value, "auditEnabled", false)?;
    require_string_array(value, "stunServers", &[""])?;
    require_i64(value, "reconnectionIntervalS", 5)?;
    require_i64(value, "maxFolderConcurrency", 1)?;
    require_string(value, "urURL", "http://127.0.0.1:1/disabled")?;
    require_string(value, "releasesURL", "http://127.0.0.1:1/disabled")?;
    require_string(value, "crURL", "http://127.0.0.1:1/disabled")
}

fn verify_devices(
    value: &Value,
    expected: &DesiredEngineConfig,
    mode: EffectiveMode,
) -> Result<(), EngineConfigError> {
    let actual = value
        .as_array()
        .ok_or(EngineConfigError::EffectiveConfigMismatch)?;
    let expected_count = 1 + usize::from(mode != EffectiveMode::Initial) * expected.peers.len();
    if actual.len() != expected_count {
        return effective_mismatch();
    }
    verify_device(
        find_unique_device(actual, expected.own_id.as_str())?,
        &expected.own_id,
        &expected.own_name,
        &["dynamic"],
        false,
    )?;
    if mode != EffectiveMode::Initial {
        for peer in &expected.peers {
            let address = format!("tcp://{}", peer.address);
            verify_device(
                find_unique_device(actual, peer.id.as_str())?,
                &peer.id,
                &peer.name,
                &[&address],
                mode == EffectiveMode::ScanGate || peer.paused,
            )?;
        }
    }
    Ok(())
}

fn find_unique_device<'a>(devices: &'a [Value], id: &str) -> Result<&'a Value, EngineConfigError> {
    let mut matching = devices
        .iter()
        .filter(|device| effective_string(device, "deviceID") == Ok(id));
    let Some(device) = matching.next() else {
        return effective_mismatch();
    };
    if matching.next().is_some() {
        return effective_mismatch();
    }
    Ok(device)
}

fn verify_device(
    value: &Value,
    id: &EngineDeviceId,
    name: &str,
    addresses: &[&str],
    paused: bool,
) -> Result<(), EngineConfigError> {
    require_string(value, "deviceID", id.as_str())?;
    require_string(value, "name", name)?;
    require_string_array(value, "addresses", addresses)?;
    require_string(value, "compression", "metadata")?;
    require_bool(value, "introducer", false)?;
    require_bool(value, "skipIntroductionRemovals", false)?;
    require_string(value, "introducedBy", "")?;
    require_bool(value, "autoAcceptFolders", false)?;
    require_bool(value, "untrusted", false)?;
    require_bool(value, "paused", paused)?;
    require_i64(value, "numConnections", 1)
}

fn verify_folders(
    value: &Value,
    expected: &DesiredEngineConfig,
    mode: EffectiveMode,
) -> Result<(), EngineConfigError> {
    let actual = value
        .as_array()
        .ok_or(EngineConfigError::EffectiveConfigMismatch)?;
    if mode == EffectiveMode::Initial {
        return if actual.is_empty() {
            Ok(())
        } else {
            effective_mismatch()
        };
    }
    if actual.len() != expected.folders.len() {
        return effective_mismatch();
    }
    for folder in &expected.folders {
        let id = folder.id.to_string();
        let mut matching = actual
            .iter()
            .filter(|candidate| effective_string(candidate, "id") == Ok(id.as_str()));
        let Some(actual_folder) = matching.next() else {
            return effective_mismatch();
        };
        if matching.next().is_some() {
            return effective_mismatch();
        }
        verify_folder(
            actual_folder,
            folder,
            mode == EffectiveMode::Desired && folder.paused,
        )?;
    }
    Ok(())
}

fn verify_folder(
    value: &Value,
    expected: &EngineFolderConfig,
    paused: bool,
) -> Result<(), EngineConfigError> {
    require_string(value, "id", &expected.id.to_string())?;
    require_string(value, "label", &expected.label)?;
    require_string(
        value,
        "path",
        expected
            .root
            .to_str()
            .ok_or(EngineConfigError::EffectiveConfigMismatch)?,
    )?;
    require_string(value, "type", "sendreceive")?;
    require_string(value, "filesystemType", "basic")?;
    require_i64(value, "rescanIntervalS", 3600)?;
    require_bool(value, "fsWatcherEnabled", true)?;
    require_i64(value, "fsWatcherDelayS", 10)?;
    require_i64(value, "fsWatcherTimeoutS", 0)?;
    require_bool(value, "ignorePerms", true)?;
    require_bool(value, "autoNormalize", true)?;
    require_bool(value, "ignoreDelete", false)?;
    require_i64(value, "maxConflicts", -1)?;
    require_bool(value, "paused", paused)?;
    require_string(value, "markerName", ".stfolder")?;
    require_bool(value, "disableFsync", false)?;
    require_string(value, "copyRangeMethod", "standard")?;
    require_bool(value, "syncOwnership", false)?;
    require_bool(value, "sendOwnership", false)?;
    require_bool(value, "syncXattrs", false)?;
    require_bool(value, "sendXattrs", false)?;
    require_bool(value, "blockIndexing", true)?;
    let minimum_free = effective_field(value, "minDiskFree")?;
    require_i64(minimum_free, "value", 1)?;
    require_string(minimum_free, "unit", "%")?;
    verify_folder_devices(effective_field(value, "devices")?, &expected.members)?;
    verify_versioning(effective_field(value, "versioning")?)
}

fn verify_folder_devices(
    value: &Value,
    expected: &[EngineDeviceId],
) -> Result<(), EngineConfigError> {
    let actual = value
        .as_array()
        .ok_or(EngineConfigError::EffectiveConfigMismatch)?;
    if actual.len() != expected.len() {
        return effective_mismatch();
    }
    for member in expected {
        let matching = actual
            .iter()
            .filter(|candidate| effective_string(candidate, "deviceID") == Ok(member.as_str()))
            .count();
        if matching != 1 {
            return effective_mismatch();
        }
    }
    for member in actual {
        require_string(member, "introducedBy", "")?;
        require_string(member, "encryptionPassword", "")?;
    }
    Ok(())
}

fn verify_versioning(value: &Value) -> Result<(), EngineConfigError> {
    require_string(value, "type", "simple")?;
    require_i64(value, "cleanupIntervalS", 3600)?;
    require_string(value, "fsPath", "")?;
    require_string(value, "fsType", "basic")?;
    let params = effective_field(value, "params")?;
    require_string(params, "keep", "100")?;
    require_string(params, "cleanoutDays", "0")
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

fn validate_listener_address(address: SocketAddr) -> Result<(), EngineConfigError> {
    if address.port() == 0 || address.ip().is_multicast() {
        return Err(EngineConfigError::InvalidAddress);
    }
    Ok(())
}

fn unacceptable_ip(address: IpAddr) -> bool {
    address.is_unspecified() || address.is_multicast()
}

fn roots_overlap(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

struct XmlWriter {
    content: String,
}

impl XmlWriter {
    fn new() -> Self {
        Self {
            content: String::new(),
        }
    }

    fn push(&mut self, value: &str) -> Result<(), EngineConfigError> {
        let length = self
            .content
            .len()
            .checked_add(value.len())
            .ok_or(EngineConfigError::LimitExceeded)?;
        if length > MAX_RENDERED_XML_BYTES {
            return Err(EngineConfigError::LimitExceeded);
        }
        self.content
            .try_reserve(value.len())
            .map_err(|_| EngineConfigError::LimitExceeded)?;
        self.content.push_str(value);
        Ok(())
    }

    fn escaped(&mut self, value: &str) -> Result<(), EngineConfigError> {
        for character in value.chars() {
            match character {
                '&' => self.push("&amp;")?,
                '<' => self.push("&lt;")?,
                '>' => self.push("&gt;")?,
                '\"' => self.push("&quot;")?,
                '\'' => self.push("&apos;")?,
                other => {
                    let mut encoded = [0_u8; 4];
                    self.push(other.encode_utf8(&mut encoded))?;
                }
            }
        }
        Ok(())
    }

    fn finish(self) -> String {
        self.content
    }
}

impl fmt::Write for XmlWriter {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        self.push(value).map_err(|_| fmt::Error)
    }
}

fn write_folder(
    xml: &mut XmlWriter,
    folder: &EngineFolderConfig,
    paused: bool,
) -> Result<(), EngineConfigError> {
    xml.push("  <folder id=\"")?;
    xml.escaped(&folder.id.to_string())?;
    xml.push("\" label=\"")?;
    xml.escaped(&folder.label)?;
    xml.push("\" path=\"")?;
    xml.escaped(
        folder
            .root
            .to_str()
            .ok_or(EngineConfigError::UnsafeFolderRoot)?,
    )?;
    xml.push("\" type=\"sendreceive\" rescanIntervalS=\"3600\" fsWatcherEnabled=\"true\" fsWatcherDelayS=\"10\" fsWatcherTimeoutS=\"0\" ignorePerms=\"true\" autoNormalize=\"true\">\n")?;
    xml.push("    <filesystemType>basic</filesystemType>\n")?;
    for member in &folder.members {
        xml.push("    <device id=\"")?;
        xml.escaped(member.as_str())?;
        xml.push("\" introducedBy=\"\"><encryptionPassword></encryptionPassword></device>\n")?;
    }
    xml.push("    <minDiskFree unit=\"%\">1</minDiskFree>\n")?;
    xml.push("    <versioning type=\"simple\">\n")?;
    xml.push("      <param key=\"keep\" val=\"100\"></param>\n")?;
    xml.push("      <param key=\"cleanoutDays\" val=\"0\"></param>\n")?;
    xml.push("      <cleanupIntervalS>3600</cleanupIntervalS>\n")?;
    xml.push("      <fsPath></fsPath>\n")?;
    xml.push("      <fsType>basic</fsType>\n")?;
    xml.push("    </versioning>\n")?;
    xml.push("    <ignoreDelete>false</ignoreDelete>\n")?;
    xml.push("    <maxConflicts>-1</maxConflicts>\n")?;
    writeln!(xml, "    <paused>{paused}</paused>").map_err(|_| EngineConfigError::RenderFailed)?;
    xml.push("    <markerName>.stfolder</markerName>\n")?;
    xml.push("    <disableFsync>false</disableFsync>\n")?;
    xml.push("    <copyRangeMethod>standard</copyRangeMethod>\n")?;
    xml.push("    <syncOwnership>false</syncOwnership>\n")?;
    xml.push("    <sendOwnership>false</sendOwnership>\n")?;
    xml.push("    <syncXattrs>false</syncXattrs>\n")?;
    xml.push("    <sendXattrs>false</sendXattrs>\n")?;
    xml.push("    <blockIndexing>true</blockIndexing>\n")?;
    xml.push("  </folder>\n")
}

fn write_device(
    xml: &mut XmlWriter,
    id: &EngineDeviceId,
    name: &str,
    address: Option<SocketAddr>,
    paused: bool,
) -> Result<(), EngineConfigError> {
    xml.push("  <device id=\"")?;
    xml.escaped(id.as_str())?;
    xml.push("\" name=\"")?;
    xml.escaped(name)?;
    xml.push("\" compression=\"metadata\" introducer=\"false\" skipIntroductionRemovals=\"false\" introducedBy=\"\">\n")?;
    xml.push("    <address>")?;
    if let Some(address) = address {
        xml.push("tcp://")?;
        write!(xml, "{address}").map_err(|_| EngineConfigError::RenderFailed)?;
    }
    xml.push("</address>\n")?;
    writeln!(xml, "    <paused>{paused}</paused>").map_err(|_| EngineConfigError::RenderFailed)?;
    xml.push("    <autoAcceptFolders>false</autoAcceptFolders>\n")?;
    xml.push("    <untrusted>false</untrusted>\n")?;
    xml.push("    <numConnections>1</numConnections>\n")?;
    xml.push("  </device>\n")
}

fn write_gui(
    xml: &mut XmlWriter,
    endpoint: &EngineGuiEndpoint,
    key: &EngineApiKey,
) -> Result<(), EngineConfigError> {
    xml.push(if endpoint.uses_tls() {
        "  <gui enabled=\"true\" tls=\"true\" sendBasicAuthPrompt=\"false\">\n"
    } else {
        "  <gui enabled=\"true\" tls=\"false\" sendBasicAuthPrompt=\"false\">\n"
    })?;
    xml.push("    <address>")?;
    xml.escaped(&endpoint.address()?)?;
    xml.push("</address>\n")?;
    xml.push("    <unixSocketPermissions>0600</unixSocketPermissions>\n")?;
    xml.push("    <metricsWithoutAuth>false</metricsWithoutAuth>\n")?;
    xml.push("    <apikey>")?;
    xml.escaped(key.expose())?;
    xml.push("</apikey>\n")?;
    xml.push("    <insecureAdminAccess>false</insecureAdminAccess>\n")?;
    xml.push("    <insecureSkipHostcheck>false</insecureSkipHostcheck>\n")?;
    xml.push("    <insecureAllowFrameLoading>false</insecureAllowFrameLoading>\n")?;
    xml.push("  </gui>\n")
}

fn write_options(
    xml: &mut XmlWriter,
    listener: Option<SocketAddr>,
) -> Result<(), EngineConfigError> {
    xml.push("  <options>\n")?;
    xml.push("    <listenAddress>")?;
    if let Some(address) = listener {
        xml.push("tcp://")?;
        write!(xml, "{address}").map_err(|_| EngineConfigError::RenderFailed)?;
    }
    xml.push("</listenAddress>\n")?;
    xml.push("    <globalAnnounceServer></globalAnnounceServer>\n")?;
    xml.push("    <globalAnnounceEnabled>false</globalAnnounceEnabled>\n")?;
    xml.push("    <localAnnounceEnabled>false</localAnnounceEnabled>\n")?;
    xml.push("    <reconnectionIntervalS>5</reconnectionIntervalS>\n")?;
    xml.push("    <relaysEnabled>false</relaysEnabled>\n")?;
    xml.push("    <startBrowser>false</startBrowser>\n")?;
    xml.push("    <natEnabled>false</natEnabled>\n")?;
    xml.push("    <urAccepted>-1</urAccepted>\n")?;
    xml.push("    <urSeen>-1</urSeen>\n")?;
    xml.push("    <urURL>http://127.0.0.1:1/disabled</urURL>\n")?;
    xml.push("    <autoUpgradeIntervalH>0</autoUpgradeIntervalH>\n")?;
    xml.push("    <upgradeToPreReleases>false</upgradeToPreReleases>\n")?;
    xml.push("    <releasesURL>http://127.0.0.1:1/disabled</releasesURL>\n")?;
    xml.push("    <maxFolderConcurrency>1</maxFolderConcurrency>\n")?;
    xml.push("    <crashReportingURL>http://127.0.0.1:1/disabled</crashReportingURL>\n")?;
    xml.push("    <crashReportingEnabled>false</crashReportingEnabled>\n")?;
    xml.push("    <stunServer></stunServer>\n")?;
    xml.push("    <announceLANAddresses>false</announceLANAddresses>\n")?;
    xml.push("    <auditEnabled>false</auditEnabled>\n")?;
    xml.push("  </options>\n")
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
