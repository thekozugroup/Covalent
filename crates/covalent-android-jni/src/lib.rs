//! Android JNI lifecycle bridge for a process-local Covalent node.
//!
//! Kotlin owns the Android Keystore wrapper and app-private no-backup directory.
//! Rust receives an exact versioned KEK into a zeroizing runtime protector; it
//! neither persists nor logs either secret. Every Java-facing result is a small structured JSON object so a
//! failure never crosses the FFI boundary as a panic.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::c_void;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use covalent_core::{ProviderQuotaPolicy, RecoveryUnlockKey, StaticKeyProtector};
use covalent_node::runtime::{
    LocalApiTokenSource, NodeRuntime, NodeRuntimeConfig, RecoveryBootstrap,
};
use covalent_node::sync_engine::{FolderSyncRuntimeConfig, VerifiedEngineExecutable};
use covalent_protocol::PlatformTier;
use jni::EnvUnowned;
use jni::objects::{JByteArray, JClass, JString};
use jni::strings::JNIString;
use jni::sys::{JNI_ERR, JNI_VERSION_1_6, jboolean, jint, jlong, jstring};
use jni::{JavaVM, NativeMethod};
use serde::Serialize;
use zeroize::{Zeroize, Zeroizing};

const NATIVE_CLASS: &str = "life/michaelwong/covalent/node/CovalentNative";
const MAX_LIVE_NODES: usize = 2;
const MIN_PROVIDER_BYTES: u64 = 256 * 1_024 * 1_024;
const MAX_PROVIDER_BYTES: u64 = 8 * 1_024 * 1_024 * 1_024 * 1_024;
const MAX_RECOVERY_KIT_BYTES: usize = 16 * 1_024 * 1_024;
const FOLDER_SYNC_PORT: u16 = 8_789;
// Android Keystore protection levels, mirroring
// `life.michaelwong.covalent.node.KeyProtectionLevel`.  Kotlin owns the probe
// because only the platform can answer it: it generates the AES-GCM protector
// key inside `AndroidKeyStore`, performs a real seal/open round trip, and reads
// the resulting `KeyInfo` security level.  Rust owns the policy, so the decision
// to run at all is made once, here, from a measured value rather than an
// assumption.  These wire values are a fixed part of the JNI contract.
const PROTECTION_UNAVAILABLE: i32 = 0;
const PROTECTION_SOFTWARE: i32 = 1;
const PROTECTION_TRUSTED_ENVIRONMENT: i32 = 2;
const PROTECTION_STRONGBOX: i32 = 3;

/// The measured Keystore protection level behind this device's identity.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum IdentityProtection {
    /// No Keystore key could be created or exercised.
    Unavailable,
    /// A Keystore key exists but the platform keeps it in software.
    Software,
    /// The key lives in the device's trusted execution environment.
    TrustedEnvironment,
    /// The key lives in a discrete StrongBox security chip.
    StrongBox,
}

impl IdentityProtection {
    /// Decodes the wire value.  Anything the current Kotlin enum does not
    /// define — a garbled, truncated, or future value — decodes to
    /// `Unavailable`, so a Kotlin-side addition can never silently widen what
    /// Rust admits.
    fn from_wire(level: i32) -> Self {
        match level {
            PROTECTION_UNAVAILABLE => Self::Unavailable,
            PROTECTION_SOFTWARE => Self::Software,
            PROTECTION_TRUSTED_ENVIRONMENT => Self::TrustedEnvironment,
            PROTECTION_STRONGBOX => Self::StrongBox,
            _ => Self::Unavailable,
        }
    }

    /// Fail-closed admission for the embedded node.  `Unavailable` means the
    /// platform could not keep the local API credential under a Keystore key at
    /// all, so the node must not start and persist long-lived state next to an
    /// unprotected credential.
    fn admits_embedded_node(self) -> bool {
        self != Self::Unavailable
    }
}

fn identity_protection_accepted(level: i32) -> bool {
    IdentityProtection::from_wire(level).admits_embedded_node()
}

struct NativeRegistry {
    runtime: Arc<tokio::runtime::Runtime>,
    nodes: BTreeMap<u64, Arc<NodeRuntime>>,
    reserved_handles: BTreeSet<u64>,
    next_handle: u64,
}

impl NativeRegistry {
    fn new() -> Result<Self, ()> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("covalent-android-node")
            .enable_io()
            .enable_time()
            .build()
            .map_err(|_| ())?;
        Ok(Self {
            runtime: Arc::new(runtime),
            nodes: BTreeMap::new(),
            reserved_handles: BTreeSet::new(),
            next_handle: 1,
        })
    }

    fn allocate_handle(&mut self) -> Result<u64, ()> {
        if self.nodes.len().saturating_add(self.reserved_handles.len()) >= MAX_LIVE_NODES {
            return Err(());
        }
        let handle = self.next_handle;
        self.next_handle = self.next_handle.checked_add(1).ok_or(())?;
        if handle == 0
            || self.nodes.contains_key(&handle)
            || self.reserved_handles.contains(&handle)
        {
            return Err(());
        }
        self.reserved_handles.insert(handle);
        Ok(handle)
    }
}

fn registry() -> Result<&'static Mutex<NativeRegistry>, ()> {
    static REGISTRY: OnceLock<Result<Mutex<NativeRegistry>, ()>> = OnceLock::new();
    match REGISTRY.get_or_init(|| NativeRegistry::new().map(Mutex::new)) {
        Ok(registry) => Ok(registry),
        Err(()) => Err(()),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NativeResponse<'a> {
    ok: bool,
    code: &'a str,
    message: &'a str,
    handle: Option<u64>,
    api_base_url: Option<String>,
    peer_address: Option<String>,
    state: &'a str,
}

impl<'a> NativeResponse<'a> {
    fn error(code: &'a str, message: &'a str) -> Self {
        Self {
            ok: false,
            code,
            message,
            handle: None,
            api_base_url: None,
            peer_address: None,
            state: "stopped",
        }
    }

    fn running(handle: u64, api_base_url: String, peer_address: String) -> Self {
        Self {
            ok: true,
            code: "ok",
            message: "Covalent's on-phone node is running.",
            handle: Some(handle),
            api_base_url: Some(api_base_url),
            peer_address: Some(peer_address),
            state: "running",
        }
    }

    fn stopped() -> Self {
        Self {
            ok: true,
            code: "ok",
            message: "Storing backups on this phone is stopped.",
            handle: None,
            api_base_url: None,
            peer_address: None,
            state: "stopped",
        }
    }
}

fn response_json(response: NativeResponse<'_>) -> String {
    serde_json::to_string(&response).unwrap_or_else(|_| {
        "{\"ok\":false,\"code\":\"serialization_failed\",\"message\":\"Storing backups on this phone is unavailable right now.\",\"state\":\"stopped\"}".to_owned()
    })
}

fn loopback_zero() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)
}

/// Peer traffic is intentionally distinct from the loopback-only management API.
/// Pairing resolves reachability from the live QUIC path rather than treating this
/// wildcard bind as a signed, reachable address.
fn wildcard_peer_zero() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)
}

fn provider_quota(
    maximum_total_bytes: u64,
    free_space_reserve_bytes: u64,
) -> Result<ProviderQuotaPolicy, ()> {
    if !(MIN_PROVIDER_BYTES..=MAX_PROVIDER_BYTES).contains(&maximum_total_bytes)
        || free_space_reserve_bytes > maximum_total_bytes.saturating_sub(MIN_PROVIDER_BYTES)
    {
        return Err(());
    }
    Ok(ProviderQuotaPolicy {
        maximum_total_bytes,
        maximum_peer_bytes: maximum_total_bytes,
        maximum_backup_bytes: maximum_total_bytes,
        free_space_reserve_bytes,
        ..ProviderQuotaPolicy::default()
    })
}

fn apply_host_runtime_flags(
    configuration: &mut NodeRuntimeConfig,
    backup_provider_enabled: bool,
    folder_sync_package_invalid: bool,
    folder_sync_access_unavailable: bool,
) {
    configuration.local_provider_enabled = backup_provider_enabled;
    configuration.folder_sync_package_invalid = folder_sync_package_invalid;
    configuration.folder_sync_access_unavailable = folder_sync_access_unavailable;
}

struct StartNodeRequest {
    data_directory: String,
    device_name: String,
    lan_discovery_enabled: bool,
    token: Zeroizing<Vec<u8>>,
    key_encryption_key: Zeroizing<Vec<u8>>,
    key_version: i32,
    maximum_total_bytes: u64,
    free_space_reserve_bytes: u64,
    key_protection_level: i32,
    backup_provider_enabled: bool,
    recovery: Option<RecoveryBootstrap>,
    folder_sync: Option<FolderSyncRuntimeConfig>,
    folder_sync_package_invalid: bool,
    folder_sync_access_unavailable: bool,
}

fn start_node(request: StartNodeRequest) -> NativeResponse<'static> {
    let StartNodeRequest {
        data_directory,
        device_name,
        lan_discovery_enabled,
        mut token,
        key_encryption_key,
        key_version,
        maximum_total_bytes,
        free_space_reserve_bytes,
        key_protection_level,
        backup_provider_enabled,
        recovery,
        folder_sync,
        folder_sync_package_invalid,
        folder_sync_access_unavailable,
    } = request;
    let recovering = recovery.is_some();
    let result = (|| {
        if !identity_protection_accepted(key_protection_level) {
            return Err("secure_key_protector_required");
        }
        if data_directory.is_empty()
            || data_directory.len() > 16_384
            || device_name.is_empty()
            || device_name.len() > 128
        {
            return Err("invalid_start_request");
        }
        let quota = provider_quota(maximum_total_bytes, free_space_reserve_bytes)
            .map_err(|_| "invalid_provider_quota")?;
        let token = Zeroizing::new(
            String::from_utf8(std::mem::take(token.as_mut())).map_err(|_| "invalid_api_token")?,
        );
        if !(32..=512).contains(&token.len()) {
            return Err("invalid_api_token");
        }
        if key_version <= 0 || key_encryption_key.len() != 32 {
            return Err("invalid_key_encryption_key");
        }
        let mut kek = [0_u8; 32];
        kek.copy_from_slice(key_encryption_key.as_ref());
        let protector = StaticKeyProtector::new(key_version as u32, kek)
            .map_err(|_| "invalid_key_encryption_key");
        kek.zeroize();
        let protector = protector?;
        let registry = registry().map_err(|_| "runtime_unavailable")?;
        let (handle, runtime) = {
            let mut registry = registry.lock().map_err(|_| "runtime_unavailable")?;
            let handle = registry
                .allocate_handle()
                .map_err(|_| "node_capacity_reached")?;
            (handle, Arc::clone(&registry.runtime))
        };
        let mut configuration = NodeRuntimeConfig::new(
            PathBuf::from(data_directory),
            loopback_zero(),
            wildcard_peer_zero(),
        );
        configuration.device_name = device_name;
        configuration.lan_discovery_enabled = lan_discovery_enabled;
        configuration.platform_tier = PlatformTier::Tier1;
        configuration.provider_quota_policy = quota;
        configuration.api_token = LocalApiTokenSource::Provided(token);
        configuration.key_protector = Some(Arc::new(protector));
        configuration.recovery = recovery;
        configuration.folder_sync = folder_sync;
        // Capture the persisted provider toggle in this launch snapshot. Folder sync and
        // owner/client backup stay available while remote chunk admission is disabled.
        apply_host_runtime_flags(
            &mut configuration,
            backup_provider_enabled,
            folder_sync_package_invalid,
            folder_sync_access_unavailable,
        );
        let node = match runtime.block_on(NodeRuntime::start(configuration)) {
            Ok(node) => node,
            Err(_) => {
                if let Ok(mut registry) = registry.lock() {
                    registry.reserved_handles.remove(&handle);
                }
                return Err(if recovering {
                    "node_recovery_failed"
                } else {
                    "node_start_failed"
                });
            }
        };
        let ready = node.ready_info();
        let response = NativeResponse::running(
            handle,
            ready.api_base_url().to_owned(),
            ready.peer_address().to_string(),
        );
        let mut registry = registry.lock().map_err(|_| "runtime_unavailable")?;
        if !registry.reserved_handles.remove(&handle) {
            drop(registry);
            let _ = runtime.block_on(node.stop());
            return Err("runtime_unavailable");
        }
        registry.nodes.insert(handle, Arc::new(node));
        Ok(response)
    })();
    match result {
        Ok(response) => response,
        Err("invalid_start_request") => NativeResponse::error(
            "invalid_start_request",
            "The storage settings for this phone are not valid.",
        ),
        Err("invalid_provider_quota") => NativeResponse::error(
            "invalid_provider_quota",
            "The size limit must leave the free space you asked to keep.",
        ),
        Err("invalid_api_token") => NativeResponse::error(
            "invalid_api_token",
            "This phone's protected server credential is not valid.",
        ),
        Err("invalid_key_encryption_key") => NativeResponse::error(
            "invalid_key_encryption_key",
            "This phone's protected storage key is not valid.",
        ),
        Err("secure_key_protector_required") => NativeResponse::error(
            "secure_key_protector_required",
            "This phone cannot protect its Covalent identity, so it cannot store backups.",
        ),
        Err("node_capacity_reached") => NativeResponse::error(
            "node_capacity_reached",
            "This phone is already storing backups.",
        ),
        Err("runtime_unavailable") => NativeResponse::error(
            "runtime_unavailable",
            "Storing backups on this phone is unavailable right now.",
        ),
        Err("node_recovery_failed") => NativeResponse::error(
            "node_recovery_failed",
            "This phone could not recover that Covalent identity. The existing identity was left unchanged.",
        ),
        Err(_) => NativeResponse::error(
            "node_start_failed",
            "This phone could not start storing backups.",
        ),
    }
}

fn stop_node(handle: u64) -> NativeResponse<'static> {
    let result = (|| {
        if handle == 0 {
            return Err("invalid_handle");
        }
        let registry = registry().map_err(|_| "runtime_unavailable")?;
        let (node, runtime) = {
            let registry = registry.lock().map_err(|_| "runtime_unavailable")?;
            let node = registry.nodes.get(&handle).cloned();
            (node, Arc::clone(&registry.runtime))
        };
        let Some(node) = node else {
            return Ok(NativeResponse::stopped());
        };
        runtime
            .block_on(node.stop())
            .map_err(|_| "node_stop_failed")?;
        let mut registry = registry.lock().map_err(|_| "runtime_unavailable")?;
        if registry
            .nodes
            .get(&handle)
            .is_some_and(|incumbent| Arc::ptr_eq(incumbent, &node))
        {
            registry.nodes.remove(&handle);
        }
        Ok(NativeResponse::stopped())
    })();
    match result {
        Ok(response) => response,
        Err("invalid_handle") => NativeResponse::error(
            "invalid_handle",
            "Storing backups on this phone is not running.",
        ),
        Err("runtime_unavailable") => NativeResponse::error(
            "runtime_unavailable",
            "Storing backups on this phone is unavailable right now.",
        ),
        Err(_) => NativeResponse::error(
            "node_stop_failed",
            "This phone could not stop storing backups.",
        ),
    }
}

fn node_state(handle: u64) -> NativeResponse<'static> {
    let result = (|| {
        if handle == 0 {
            return Err("invalid_handle");
        }
        let registry = registry().map_err(|_| "runtime_unavailable")?;
        let registry = registry.lock().map_err(|_| "runtime_unavailable")?;
        let Some(node) = registry.nodes.get(&handle) else {
            return Ok(NativeResponse::stopped());
        };
        let ready = node.ready_info();
        Ok(NativeResponse::running(
            handle,
            ready.api_base_url().to_owned(),
            ready.peer_address().to_string(),
        ))
    })();
    match result {
        Ok(response) => response,
        Err("invalid_handle") => NativeResponse::error(
            "invalid_handle",
            "Storing backups on this phone is not running.",
        ),
        Err(_) => NativeResponse::error(
            "runtime_unavailable",
            "Storing backups on this phone is unavailable right now.",
        ),
    }
}

fn with_java_response<'local>(
    mut unowned: EnvUnowned<'local>,
    build: impl FnOnce(&mut jni::Env<'_>) -> NativeResponse<'static>,
) -> jstring {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        unowned.with_env(|environment| {
            let response = build(environment);
            environment
                .new_string(response_json(response))
                .map(JString::into_raw)
        })
    }));
    match outcome {
        Ok(outcome) => outcome.resolve::<jni::errors::ThrowRuntimeExAndDefault>(),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Copies a JVM secret into zeroizing Rust memory and immediately clears the JVM array.
/// A wipe failure is a locked result: startup must not proceed with an extra live copy.
fn take_java_secret(
    environment: &jni::Env<'_>,
    array: &JByteArray<'_>,
) -> Result<Zeroizing<Vec<u8>>, ()> {
    let secret = environment
        .convert_byte_array(array)
        .map(Zeroizing::new)
        .map_err(|_| ());
    let length = array.len(environment).map_err(|_| ())?;
    let zeros = [0_i8; 64];
    let mut offset = 0_usize;
    while offset < length {
        let count = (length - offset).min(zeros.len());
        array
            .set_region(environment, offset as i32, &zeros[..count])
            .map_err(|_| ())?;
        offset += count;
    }
    secret
}

fn recovery_bootstrap(
    mut kit: Zeroizing<Vec<u8>>,
    key: Zeroizing<Vec<u8>>,
) -> Result<RecoveryBootstrap, ()> {
    if !(1..=MAX_RECOVERY_KIT_BYTES).contains(&kit.len()) || key.len() != 32 {
        return Err(());
    }
    let mut raw_key = [0_u8; 32];
    raw_key.copy_from_slice(key.as_ref());
    let unlock = RecoveryUnlockKey::from_bytes(raw_key);
    raw_key.zeroize();
    Ok(RecoveryBootstrap {
        kit: Zeroizing::new(std::mem::take(kit.as_mut())),
        unlock,
    })
}

fn packaged_folder_sync(
    package_invalid: bool,
    guardian_path: String,
    guardian_sha256: String,
    worker_path: String,
    worker_sha256: String,
    runtime_directory: String,
    listener_port: jint,
) -> (Option<FolderSyncRuntimeConfig>, bool) {
    let values = [
        guardian_path.as_str(),
        guardian_sha256.as_str(),
        worker_path.as_str(),
        worker_sha256.as_str(),
        runtime_directory.as_str(),
    ];
    if package_invalid {
        return (None, true);
    }
    if values.iter().all(|value| value.is_empty()) {
        return (None, false);
    }
    if values
        .iter()
        .any(|value| value.is_empty() || value.len() > 16_384)
    {
        return (None, true);
    }
    let Some(listener_port) = valid_listener_port(listener_port) else {
        return (None, true);
    };
    let Ok(guardian_digest) = VerifiedEngineExecutable::parse_sha256_hex(&guardian_sha256) else {
        return (None, true);
    };
    let Ok(worker_digest) = VerifiedEngineExecutable::parse_sha256_hex(&worker_sha256) else {
        return (None, true);
    };
    let Ok(guardian) =
        VerifiedEngineExecutable::open(PathBuf::from(guardian_path), guardian_digest)
    else {
        return (None, true);
    };
    let Ok(worker) = VerifiedEngineExecutable::open(PathBuf::from(worker_path), worker_digest)
    else {
        return (None, true);
    };
    (
        Some(FolderSyncRuntimeConfig {
            guardian,
            worker,
            runtime_parent: PathBuf::from(runtime_directory),
            listener: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), listener_port),
            advertised_address: None,
        }),
        false,
    )
}

fn valid_listener_port(port: jint) -> Option<u16> {
    u16::try_from(port).ok().filter(|port| *port != 0)
}

extern "system" fn native_start<'local>(
    unowned: EnvUnowned<'local>,
    _class: JClass<'local>,
    data_directory: JString<'local>,
    device_name: JString<'local>,
    lan_discovery_enabled: jboolean,
    api_token: JByteArray<'local>,
    key_encryption_key: JByteArray<'local>,
    key_version: jint,
    maximum_total_bytes: jlong,
    free_space_reserve_bytes: jlong,
    key_protection_level: jint,
    backup_provider_enabled: jboolean,
    sync_package_invalid: jboolean,
    folder_sync_access_unavailable: jboolean,
    sync_guardian_path: JString<'local>,
    sync_guardian_sha256: JString<'local>,
    sync_worker_path: JString<'local>,
    sync_worker_sha256: JString<'local>,
    sync_runtime_directory: JString<'local>,
    sync_listener_port: jint,
) -> jstring {
    with_java_response(unowned, |environment| {
        let data_directory = data_directory.to_string();
        let device_name = device_name.to_string();
        let token = take_java_secret(environment, &api_token);
        let key = take_java_secret(environment, &key_encryption_key);
        let (folder_sync, folder_sync_package_invalid) = packaged_folder_sync(
            sync_package_invalid,
            sync_guardian_path.to_string(),
            sync_guardian_sha256.to_string(),
            sync_worker_path.to_string(),
            sync_worker_sha256.to_string(),
            sync_runtime_directory.to_string(),
            sync_listener_port,
        );
        match (token, key) {
            (Ok(token), Ok(key)) if maximum_total_bytes > 0 && free_space_reserve_bytes >= 0 => {
                start_node(StartNodeRequest {
                    data_directory,
                    device_name,
                    lan_discovery_enabled,
                    token,
                    key_encryption_key: key,
                    key_version,
                    maximum_total_bytes: maximum_total_bytes as u64,
                    free_space_reserve_bytes: free_space_reserve_bytes as u64,
                    key_protection_level,
                    backup_provider_enabled,
                    recovery: None,
                    folder_sync,
                    folder_sync_package_invalid,
                    folder_sync_access_unavailable,
                })
            }
            _ => NativeResponse::error(
                "invalid_start_request",
                "The storage settings for this phone are not valid.",
            ),
        }
    })
}

extern "system" fn native_recover_start<'local>(
    unowned: EnvUnowned<'local>,
    _class: JClass<'local>,
    data_directory: JString<'local>,
    device_name: JString<'local>,
    lan_discovery_enabled: jboolean,
    api_token: JByteArray<'local>,
    key_encryption_key: JByteArray<'local>,
    key_version: jint,
    maximum_total_bytes: jlong,
    free_space_reserve_bytes: jlong,
    key_protection_level: jint,
    recovery_kit: JByteArray<'local>,
    recovery_key: JByteArray<'local>,
    backup_provider_enabled: jboolean,
    sync_package_invalid: jboolean,
    folder_sync_access_unavailable: jboolean,
    sync_guardian_path: JString<'local>,
    sync_guardian_sha256: JString<'local>,
    sync_worker_path: JString<'local>,
    sync_worker_sha256: JString<'local>,
    sync_runtime_directory: JString<'local>,
) -> jstring {
    with_java_response(unowned, |environment| {
        let data_directory = data_directory.to_string();
        let device_name = device_name.to_string();
        // Consume every secret array before inspecting any value. A malformed token or
        // configuration must not leave the recovery key live in the JVM array.
        let token = take_java_secret(environment, &api_token);
        let key_encryption_key = take_java_secret(environment, &key_encryption_key);
        let recovery_kit = take_java_secret(environment, &recovery_kit);
        let recovery_key = take_java_secret(environment, &recovery_key);
        let (folder_sync, folder_sync_package_invalid) = packaged_folder_sync(
            sync_package_invalid,
            sync_guardian_path.to_string(),
            sync_guardian_sha256.to_string(),
            sync_worker_path.to_string(),
            sync_worker_sha256.to_string(),
            sync_runtime_directory.to_string(),
            FOLDER_SYNC_PORT.into(),
        );
        match (token, key_encryption_key, recovery_kit, recovery_key) {
            (Ok(token), Ok(key_encryption_key), Ok(recovery_kit), Ok(recovery_key))
                if maximum_total_bytes > 0 && free_space_reserve_bytes >= 0 =>
            {
                let recovery = match recovery_bootstrap(recovery_kit, recovery_key) {
                    Ok(recovery) => recovery,
                    Err(()) => {
                        return NativeResponse::error(
                            "invalid_recovery_request",
                            "The selected recovery kit or recovery code is invalid.",
                        );
                    }
                };
                start_node(StartNodeRequest {
                    data_directory,
                    device_name,
                    lan_discovery_enabled,
                    token,
                    key_encryption_key,
                    key_version,
                    maximum_total_bytes: maximum_total_bytes as u64,
                    free_space_reserve_bytes: free_space_reserve_bytes as u64,
                    key_protection_level,
                    backup_provider_enabled,
                    recovery: Some(recovery),
                    folder_sync,
                    folder_sync_package_invalid,
                    folder_sync_access_unavailable,
                })
            }
            _ => NativeResponse::error(
                "invalid_recovery_request",
                "The selected recovery kit or recovery code is invalid.",
            ),
        }
    })
}

extern "system" fn native_stop<'local>(
    unowned: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
) -> jstring {
    with_java_response(unowned, |_environment| {
        if handle < 0 {
            NativeResponse::error(
                "invalid_handle",
                "Storing backups on this phone is not running.",
            )
        } else {
            stop_node(handle as u64)
        }
    })
}

extern "system" fn native_state<'local>(
    unowned: EnvUnowned<'local>,
    _class: JClass<'local>,
    handle: jlong,
) -> jstring {
    with_java_response(unowned, |_environment| {
        if handle < 0 {
            NativeResponse::error(
                "invalid_handle",
                "Storing backups on this phone is not running.",
            )
        } else {
            node_state(handle as u64)
        }
    })
}

/// Registers the fixed Kotlin ABI.  A failed registration leaves the library unusable.
///
/// # Safety
///
/// This is a JVM entry point and must only be invoked by the JVM itself, which
/// guarantees `vm` is a non-null, valid, currently-attached `JavaVM` pointer.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn JNI_OnLoad(
    vm: *mut jni::sys::JavaVM,
    _: *mut c_void,
) -> jni::sys::jint {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: the JVM invokes JNI_OnLoad with a non-null, valid JavaVM pointer.
        let vm = unsafe { JavaVM::from_raw(vm) };
        vm.with_top_local_frame(|environment| {
            let class = environment.find_class(JNIString::from(NATIVE_CLASS))?;
            let native_start_name = JNIString::from("nativeStart");
            let native_start_signature = JNIString::from(
                "(Ljava/lang/String;Ljava/lang/String;Z[B[BIJJIZZZLjava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;I)Ljava/lang/String;",
            );
            let native_recover_start_name = JNIString::from("nativeRecoverStart");
            let native_recover_start_signature = JNIString::from(
                "(Ljava/lang/String;Ljava/lang/String;Z[B[BIJJI[B[BZZZLjava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;",
            );
            let native_stop_name = JNIString::from("nativeStop");
            let native_stop_signature = JNIString::from("(J)Ljava/lang/String;");
            let native_state_name = JNIString::from("nativeState");
            let native_state_signature = JNIString::from("(J)Ljava/lang/String;");
            let methods = [
                // SAFETY: signature exactly matches the static Kotlin start declaration.
                unsafe {
                    NativeMethod::from_raw_parts(
                        &native_start_name,
                        &native_start_signature,
                        native_start as *mut c_void,
                    )
                },
                // SAFETY: signature exactly matches the static Kotlin recovery declaration.
                unsafe {
                    NativeMethod::from_raw_parts(
                        &native_recover_start_name,
                        &native_recover_start_signature,
                        native_recover_start as *mut c_void,
                    )
                },
                // SAFETY: signature exactly matches the static Kotlin extern declaration.
                unsafe {
                    NativeMethod::from_raw_parts(
                        &native_stop_name,
                        &native_stop_signature,
                        native_stop as *mut c_void,
                    )
                },
                // SAFETY: signature exactly matches the static Kotlin extern declaration.
                unsafe {
                    NativeMethod::from_raw_parts(
                        &native_state_name,
                        &native_state_signature,
                        native_state as *mut c_void,
                    )
                },
            ];
            // SAFETY: class is the Kotlin object class and all descriptors above are exact.
            unsafe { environment.register_native_methods(class, &methods) }
        })
    }));
    match outcome {
        Ok(Ok(())) => JNI_VERSION_1_6,
        _ => JNI_ERR,
    }
}

#[cfg(test)]
mod tests {
    use zeroize::Zeroizing;

    use super::{
        FOLDER_SYNC_PORT, IdentityProtection, MAX_RECOVERY_KIT_BYTES, NativeRegistry,
        PROTECTION_SOFTWARE, PROTECTION_STRONGBOX, PROTECTION_TRUSTED_ENVIRONMENT,
        PROTECTION_UNAVAILABLE, apply_host_runtime_flags, identity_protection_accepted,
        loopback_zero, packaged_folder_sync, provider_quota, recovery_bootstrap,
        valid_listener_port, wildcard_peer_zero,
    };
    use covalent_node::runtime::NodeRuntimeConfig;
    use std::path::PathBuf;

    #[test]
    fn identity_protection_decodes_every_contract_level() {
        assert_eq!(
            IdentityProtection::from_wire(PROTECTION_UNAVAILABLE),
            IdentityProtection::Unavailable
        );
        assert_eq!(
            IdentityProtection::from_wire(PROTECTION_SOFTWARE),
            IdentityProtection::Software
        );
        assert_eq!(
            IdentityProtection::from_wire(PROTECTION_TRUSTED_ENVIRONMENT),
            IdentityProtection::TrustedEnvironment
        );
        assert_eq!(
            IdentityProtection::from_wire(PROTECTION_STRONGBOX),
            IdentityProtection::StrongBox
        );
    }

    #[test]
    fn identity_protection_admits_only_keystore_backed_levels() {
        assert!(!identity_protection_accepted(PROTECTION_UNAVAILABLE));
        assert!(identity_protection_accepted(PROTECTION_SOFTWARE));
        assert!(identity_protection_accepted(PROTECTION_TRUSTED_ENVIRONMENT));
        assert!(identity_protection_accepted(PROTECTION_STRONGBOX));
    }

    #[test]
    fn identity_protection_rejects_levels_outside_the_contract() {
        // A garbled, truncated, or future Kotlin value must fail closed rather
        // than being read as "at least as good as software protection".
        for level in [-1, 4, i32::MIN, i32::MAX] {
            assert_eq!(
                IdentityProtection::from_wire(level),
                IdentityProtection::Unavailable
            );
            assert!(!identity_protection_accepted(level));
        }
    }

    #[test]
    fn embedded_node_start_refuses_unprotected_identity() {
        // The end-to-end guard: a start request that is valid in every other
        // respect must still be refused when the identity is unprotected.
        let response = super::start_node(super::StartNodeRequest {
            data_directory: "/data/user/0/life.michaelwong.covalent/no_backup/covalent-node"
                .to_owned(),
            device_name: "Pixel Android".to_owned(),
            lan_discovery_enabled: false,
            token: Zeroizing::new(vec![b'a'; 43]),
            key_encryption_key: Zeroizing::new(vec![0x5a; 32]),
            key_version: 1,
            maximum_total_bytes: 2 * 1_024 * 1_024 * 1_024,
            free_space_reserve_bytes: 512 * 1_024 * 1_024,
            key_protection_level: PROTECTION_UNAVAILABLE,
            backup_provider_enabled: true,
            recovery: None,
            folder_sync: None,
            folder_sync_package_invalid: false,
            folder_sync_access_unavailable: false,
        });
        assert!(!response.ok);
        assert_eq!(response.code, "secure_key_protector_required");
    }

    #[test]
    fn embedded_node_start_requires_an_exact_versioned_256_bit_kek() {
        for (key, version) in [
            (vec![0x5a; 31], 1),
            (vec![0x5a; 33], 1),
            (vec![0x5a; 32], 0),
        ] {
            let response = super::start_node(super::StartNodeRequest {
                data_directory: "/data/user/0/life.michaelwong.covalent/no_backup/covalent-node"
                    .to_owned(),
                device_name: "Pixel Android".to_owned(),
                lan_discovery_enabled: false,
                token: Zeroizing::new(vec![b'a'; 43]),
                key_encryption_key: Zeroizing::new(key),
                key_version: version,
                maximum_total_bytes: 2 * 1_024 * 1_024 * 1_024,
                free_space_reserve_bytes: 512 * 1_024 * 1_024,
                key_protection_level: PROTECTION_SOFTWARE,
                backup_provider_enabled: true,
                recovery: None,
                folder_sync: None,
                folder_sync_package_invalid: false,
                folder_sync_access_unavailable: false,
            });
            assert!(!response.ok);
            assert_eq!(response.code, "invalid_key_encryption_key");
        }
    }

    #[test]
    fn recovery_bootstrap_requires_raw_bounded_kit_and_exact_256_bit_key() {
        assert!(recovery_bootstrap(Zeroizing::new(vec![1]), Zeroizing::new(vec![7; 32])).is_ok());
        for (kit_size, key_size) in [(0, 32), (1, 31), (1, 33), (MAX_RECOVERY_KIT_BYTES + 1, 32)] {
            assert!(
                recovery_bootstrap(
                    Zeroizing::new(vec![1; kit_size]),
                    Zeroizing::new(vec![7; key_size]),
                )
                .is_err()
            );
        }
    }

    #[test]
    fn packaged_folder_sync_distinguishes_absent_from_invalid_input() {
        let empty = || String::new();
        let (absent, absent_invalid) = packaged_folder_sync(
            false,
            empty(),
            empty(),
            empty(),
            empty(),
            empty(),
            FOLDER_SYNC_PORT.into(),
        );
        assert!(absent.is_none());
        assert!(!absent_invalid);

        let (declared_invalid, invalid) = packaged_folder_sync(
            true,
            empty(),
            empty(),
            empty(),
            empty(),
            empty(),
            FOLDER_SYNC_PORT.into(),
        );
        assert!(declared_invalid.is_none());
        assert!(invalid);

        let (partial, partial_invalid) = packaged_folder_sync(
            false,
            "/installed/libengineguardian.so".to_owned(),
            empty(),
            empty(),
            empty(),
            empty(),
            FOLDER_SYNC_PORT.into(),
        );
        assert!(partial.is_none());
        assert!(partial_invalid);
    }

    #[test]
    fn folder_sync_listener_port_requires_nonzero_u16() {
        assert_eq!(valid_listener_port(1), Some(1));
        assert_eq!(valid_listener_port(i32::from(u16::MAX)), Some(u16::MAX));
        for rejected in [-1, 0, i32::from(u16::MAX) + 1, i32::MAX] {
            assert_eq!(valid_listener_port(rejected), None);
        }
    }

    #[test]
    fn provider_quota_rejects_unsafe_bounds() {
        assert!(provider_quota(0, 0).is_err());
        assert!(provider_quota(256 * 1_024 * 1_024, 1).is_err());
        assert!(provider_quota(512 * 1_024 * 1_024, 0).is_ok());
    }

    #[test]
    fn host_flags_keep_provider_and_folder_access_policies_independent() {
        let mut configuration = NodeRuntimeConfig::new(
            PathBuf::from("private-node"),
            loopback_zero(),
            wildcard_peer_zero(),
        );
        apply_host_runtime_flags(&mut configuration, false, true, true);
        assert!(!configuration.local_provider_enabled);
        assert!(configuration.folder_sync_package_invalid);
        assert!(configuration.folder_sync_access_unavailable);
    }

    #[test]
    fn registry_handles_are_nonzero_and_monotonic() {
        let mut registry = NativeRegistry::new().expect("runtime");
        let first = registry.allocate_handle().expect("first handle");
        let second = registry.allocate_handle().expect("second handle");
        assert!(first > 0 && second > first);
    }
}
