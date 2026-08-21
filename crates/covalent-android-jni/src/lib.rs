//! Android JNI lifecycle bridge for a process-local Covalent node.
//!
//! Kotlin owns the Android Keystore token and app-private no-backup directory.
//! Rust owns only bounded live node handles; it neither persists nor logs the
//! token.  Every Java-facing result is a small structured JSON object so a
//! failure never crosses the FFI boundary as a panic.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::c_void;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use covalent_core::ProviderQuotaPolicy;
use covalent_node::runtime::{LocalApiTokenSource, NodeRuntime, NodeRuntimeConfig};
use covalent_protocol::PlatformTier;
use jni::EnvUnowned;
use jni::objects::{JByteArray, JClass, JString};
use jni::strings::JNIString;
use jni::sys::{JNI_ERR, JNI_VERSION_1_6, jboolean, jlong, jstring};
use jni::{JavaVM, NativeMethod};
use serde::Serialize;
use zeroize::{Zeroize, Zeroizing};

const NATIVE_CLASS: &str = "life/michaelwong/covalent/node/CovalentNative";
const MAX_LIVE_NODES: usize = 2;
const MIN_PROVIDER_BYTES: u64 = 256 * 1_024 * 1_024;
const MAX_PROVIDER_BYTES: u64 = 8 * 1_024 * 1_024 * 1_024 * 1_024;
// Engine/NodeRuntime has not yet accepted the Android Keystore-backed identity
// protector. Starting before that exists would persist long-lived material with
// filesystem permissions only, so Android local-node mode stays fail-closed.
const SECURE_IDENTITY_PROTECTOR_AVAILABLE: bool = false;

struct NativeRegistry {
    runtime: Arc<tokio::runtime::Runtime>,
    nodes: BTreeMap<u64, NodeRuntime>,
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
            message: "Embedded node is running.",
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
            message: "Embedded node is stopped.",
            handle: None,
            api_base_url: None,
            peer_address: None,
            state: "stopped",
        }
    }
}

fn response_json(response: NativeResponse<'_>) -> String {
    serde_json::to_string(&response).unwrap_or_else(|_| {
        "{\"ok\":false,\"code\":\"serialization_failed\",\"message\":\"Native response is unavailable.\",\"state\":\"stopped\"}".to_owned()
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

fn start_node(
    data_directory: String,
    device_name: String,
    lan_discovery_enabled: bool,
    mut token: Vec<u8>,
    maximum_total_bytes: u64,
    free_space_reserve_bytes: u64,
) -> NativeResponse<'static> {
    let result = (|| {
        if !SECURE_IDENTITY_PROTECTOR_AVAILABLE {
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
            String::from_utf8(std::mem::take(&mut token)).map_err(|_| "invalid_api_token")?,
        );
        if !(32..=512).contains(&token.len()) {
            return Err("invalid_api_token");
        }
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
        let node = match runtime.block_on(NodeRuntime::start(configuration)) {
            Ok(node) => node,
            Err(_) => {
                if let Ok(mut registry) = registry.lock() {
                    registry.reserved_handles.remove(&handle);
                }
                return Err("node_start_failed");
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
        registry.nodes.insert(handle, node);
        Ok(response)
    })();
    token.zeroize();
    match result {
        Ok(response) => response,
        Err("invalid_start_request") => NativeResponse::error(
            "invalid_start_request",
            "Embedded node settings are invalid.",
        ),
        Err("invalid_provider_quota") => NativeResponse::error(
            "invalid_provider_quota",
            "Provider capacity must leave the requested protected free space.",
        ),
        Err("invalid_api_token") => NativeResponse::error(
            "invalid_api_token",
            "The protected local API token is invalid.",
        ),
        Err("secure_key_protector_required") => NativeResponse::error(
            "secure_key_protector_required",
            "Embedded provider needs Android Keystore protection before it can start.",
        ),
        Err("node_capacity_reached") => NativeResponse::error(
            "node_capacity_reached",
            "Too many embedded nodes are already running.",
        ),
        Err("runtime_unavailable") => NativeResponse::error(
            "runtime_unavailable",
            "The embedded runtime is unavailable.",
        ),
        Err(_) => NativeResponse::error("node_start_failed", "The embedded node could not start."),
    }
}

fn stop_node(handle: u64) -> NativeResponse<'static> {
    let result = (|| {
        if handle == 0 {
            return Err("invalid_handle");
        }
        let registry = registry().map_err(|_| "runtime_unavailable")?;
        let (node, runtime) = {
            let mut registry = registry.lock().map_err(|_| "runtime_unavailable")?;
            let node = registry.nodes.remove(&handle);
            (node, Arc::clone(&registry.runtime))
        };
        let Some(node) = node else {
            return Ok(NativeResponse::stopped());
        };
        runtime
            .block_on(node.stop())
            .map_err(|_| "node_stop_failed")?;
        Ok(NativeResponse::stopped())
    })();
    match result {
        Ok(response) => response,
        Err("invalid_handle") => {
            NativeResponse::error("invalid_handle", "Embedded node handle is invalid.")
        }
        Err("runtime_unavailable") => NativeResponse::error(
            "runtime_unavailable",
            "The embedded runtime is unavailable.",
        ),
        Err(_) => NativeResponse::error("node_stop_failed", "The embedded node could not stop."),
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
        Err("invalid_handle") => {
            NativeResponse::error("invalid_handle", "Embedded node handle is invalid.")
        }
        Err(_) => NativeResponse::error(
            "runtime_unavailable",
            "The embedded runtime is unavailable.",
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

extern "system" fn native_start<'local>(
    unowned: EnvUnowned<'local>,
    _class: JClass<'local>,
    data_directory: JString<'local>,
    device_name: JString<'local>,
    lan_discovery_enabled: jboolean,
    api_token: JByteArray<'local>,
    maximum_total_bytes: jlong,
    free_space_reserve_bytes: jlong,
) -> jstring {
    with_java_response(unowned, |environment| {
        let data_directory = data_directory.to_string();
        let device_name = device_name.to_string();
        let token = environment.convert_byte_array(&api_token);
        match token {
            Ok(token) if maximum_total_bytes > 0 && free_space_reserve_bytes >= 0 => start_node(
                data_directory,
                device_name,
                lan_discovery_enabled,
                token,
                maximum_total_bytes as u64,
                free_space_reserve_bytes as u64,
            ),
            _ => NativeResponse::error(
                "invalid_start_request",
                "Embedded node settings are invalid.",
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
            NativeResponse::error("invalid_handle", "Embedded node handle is invalid.")
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
            NativeResponse::error("invalid_handle", "Embedded node handle is invalid.")
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
            let native_start_signature =
                JNIString::from("(Ljava/lang/String;Ljava/lang/String;Z[BJJ)Ljava/lang/String;");
            let native_stop_name = JNIString::from("nativeStop");
            let native_stop_signature = JNIString::from("(J)Ljava/lang/String;");
            let native_state_name = JNIString::from("nativeState");
            let native_state_signature = JNIString::from("(J)Ljava/lang/String;");
            let methods = [
                // SAFETY: signatures exactly match the three static Kotlin extern declarations.
                unsafe {
                    NativeMethod::from_raw_parts(
                        &native_start_name,
                        &native_start_signature,
                        native_start as *mut c_void,
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
    use super::{NativeRegistry, provider_quota};

    #[test]
    fn provider_quota_rejects_unsafe_bounds() {
        assert!(provider_quota(0, 0).is_err());
        assert!(provider_quota(256 * 1_024 * 1_024, 1).is_err());
        assert!(provider_quota(512 * 1_024 * 1_024, 0).is_ok());
    }

    #[test]
    fn registry_handles_are_nonzero_and_monotonic() {
        let mut registry = NativeRegistry::new().expect("runtime");
        let first = registry.allocate_handle().expect("first handle");
        let second = registry.allocate_handle().expect("second handle");
        assert!(first > 0 && second > first);
    }
}
