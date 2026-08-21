//! Opt-in minimal LAN advertisements and untrusted Tailnet candidate discovery.

use std::collections::BTreeSet;
use std::env;
use std::io::{Read as _, Write as _};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use covalent_core::CoreError;
use covalent_protocol::PROTOCOL_VERSION;
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};

const SERVICE_TYPE: &str = "_covalent._udp.local.";
const MAX_DISCOVERY_RESULTS: usize = 256;
const MAX_TAILSCALE_STATUS_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_TAILSCALE_HTTP_BYTES: usize = MAX_TAILSCALE_STATUS_BYTES + 64 * 1_024;
const DEFAULT_TAILSCALE_SOCKET: &str = "/var/run/tailscale/tailscaled.sock";

/// Untrusted connection hint. Pairing identity validation remains mandatory.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiscoveryCandidate {
    /// Ephemeral advertisement identifier, never a stable device ID.
    pub service_id: String,
    /// Candidate address and advertised peer port.
    pub endpoint: SocketAddr,
    /// Lowest advertised protocol version.
    pub minimum_protocol_version: u16,
    /// Highest advertised protocol version.
    pub maximum_protocol_version: u16,
    /// Discovery source for UI disclosure.
    pub source: DiscoverySource,
}

/// Candidate routing source.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoverySource {
    /// Local multicast DNS.
    LanMdns,
    /// Local Tailscale CLI status.
    Tailscale,
}

/// LAN advertiser that exists only while the persisted preference is enabled.
pub struct LanDiscovery {
    daemon: Option<ServiceDaemon>,
    service_fullname: Option<String>,
}

impl LanDiscovery {
    /// Starts a minimal ephemeral advertisement, or performs no network action when disabled.
    pub fn start(enabled: bool, peer_port: u16) -> Result<Self, CoreError> {
        if !enabled {
            return Ok(Self {
                daemon: None,
                service_fullname: None,
            });
        }
        let mut random = [0_u8; 12];
        OsRng.fill_bytes(&mut random);
        let service_id = URL_SAFE_NO_PAD.encode(random).to_ascii_lowercase();
        let instance = format!("covalent-{service_id}");
        let host = format!("{instance}.local.");
        let protocol = PROTOCOL_VERSION.to_string();
        let properties = [
            ("min", protocol.as_str()),
            ("max", protocol.as_str()),
            ("caps", "chunks"),
        ];
        let info = ServiceInfo::new(
            SERVICE_TYPE,
            &instance,
            &host,
            "",
            peer_port,
            &properties[..],
        )
        .map_err(|error| CoreError::InvalidState(format!("create mDNS service: {error}")))?
        .enable_addr_auto();
        let fullname = info.get_fullname().to_owned();
        let daemon = ServiceDaemon::new()
            .map_err(|error| CoreError::InvalidState(format!("start mDNS daemon: {error}")))?;
        daemon
            .register(info)
            .map_err(|error| CoreError::InvalidState(format!("register mDNS service: {error}")))?;
        Ok(Self {
            daemon: Some(daemon),
            service_fullname: Some(fullname),
        })
    }

    /// Whether multicast advertisement is currently active.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.daemon.is_some()
    }

    /// Explicitly unregisters and shuts down multicast activity.
    pub fn stop(mut self) {
        if let (Some(daemon), Some(fullname)) = (&self.daemon, &self.service_fullname) {
            let _ = daemon.unregister(fullname);
            let _ = daemon.shutdown();
        }
        self.daemon = None;
        self.service_fullname = None;
    }

    /// Browses briefly for untrusted LAN hints only when explicitly enabled.
    pub fn browse(enabled: bool, duration: Duration) -> Result<Vec<DiscoveryCandidate>, CoreError> {
        if !enabled {
            return Ok(Vec::new());
        }
        let daemon = ServiceDaemon::new()
            .map_err(|error| CoreError::InvalidState(format!("start mDNS browser: {error}")))?;
        let receiver = daemon
            .browse(SERVICE_TYPE)
            .map_err(|error| CoreError::InvalidState(format!("browse mDNS: {error}")))?;
        let deadline = Instant::now() + duration.min(Duration::from_secs(10));
        let mut candidates = BTreeSet::new();
        while Instant::now() < deadline && candidates.len() < MAX_DISCOVERY_RESULTS {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if let Ok(ServiceEvent::ServiceResolved(info)) =
                receiver.recv_timeout(remaining.min(Duration::from_millis(250)))
            {
                let minimum = property_u16(&info, "min").unwrap_or(PROTOCOL_VERSION);
                let maximum = property_u16(&info, "max").unwrap_or(PROTOCOL_VERSION);
                for address in info.get_addresses() {
                    candidates.insert((
                        info.get_fullname().to_owned(),
                        SocketAddr::new(address.to_ip_addr(), info.get_port()),
                        minimum,
                        maximum,
                    ));
                }
            }
        }
        let _ = daemon.stop_browse(SERVICE_TYPE);
        let _ = daemon.shutdown();
        Ok(candidates
            .into_iter()
            .map(
                |(service_id, endpoint, minimum_protocol_version, maximum_protocol_version)| {
                    DiscoveryCandidate {
                        service_id,
                        endpoint,
                        minimum_protocol_version,
                        maximum_protocol_version,
                        source: DiscoverySource::LanMdns,
                    }
                },
            )
            .collect())
    }
}

impl Drop for LanDiscovery {
    fn drop(&mut self) {
        if let (Some(daemon), Some(fullname)) = (&self.daemon, &self.service_fullname) {
            let _ = daemon.unregister(fullname);
            let _ = daemon.shutdown();
        }
    }
}

/// Reconfigures the live mDNS advertiser when the persisted privacy setting changes.
pub struct DiscoveryController {
    peer_port: u16,
    current: Mutex<LanDiscovery>,
}

impl DiscoveryController {
    /// Starts the controller in the persisted state.
    pub fn new(enabled: bool, peer_port: u16) -> Result<Self, CoreError> {
        Ok(Self {
            peer_port,
            current: Mutex::new(LanDiscovery::start(enabled, peer_port)?),
        })
    }

    /// Applies a setting change immediately. A failed enable leaves the old state active.
    pub fn set_enabled(&self, enabled: bool) -> Result<(), CoreError> {
        let mut current = self
            .current
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        if current.is_active() == enabled {
            return Ok(());
        }
        let replacement = LanDiscovery::start(enabled, self.peer_port)?;
        let previous = std::mem::replace(&mut *current, replacement);
        drop(current);
        previous.stop();
        Ok(())
    }

    /// Whether the network-visible advertisement is active now.
    pub fn is_active(&self) -> Result<bool, CoreError> {
        Ok(self
            .current
            .lock()
            .map_err(|_| CoreError::Synchronization)?
            .is_active())
    }
}

/// Reads bounded Tailscale LocalAPI or CLI status as routing hints. No Tailscale identity is trusted.
pub fn discover_tailscale_candidates(peer_port: u16) -> Result<Vec<DiscoveryCandidate>, CoreError> {
    if let Some(bytes) = tailscale_localapi_status()? {
        return parse_tailscale_status(&bytes, peer_port);
    }
    let output = match Command::new("tailscale")
        .args(["status", "--json", "--timeout=2s"])
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(CoreError::Io {
                operation: "query Tailscale status",
                path: "tailscale".into(),
                source,
            });
        }
    };
    if !output.status.success() {
        return Ok(Vec::new());
    }
    if output.stdout.len() > MAX_TAILSCALE_STATUS_BYTES {
        return Err(CoreError::ResourceLimit("Tailscale status"));
    }
    parse_tailscale_status(&output.stdout, peer_port)
}

#[cfg(unix)]
fn tailscale_localapi_status() -> Result<Option<Vec<u8>>, CoreError> {
    let socket = env::var_os("COVALENT_TAILSCALE_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_TAILSCALE_SOCKET));
    tailscale_localapi_status_at(&socket)
}

#[cfg(unix)]
fn tailscale_localapi_status_at(socket: &std::path::Path) -> Result<Option<Vec<u8>>, CoreError> {
    use std::os::unix::net::UnixStream;

    let mut stream = match UnixStream::connect(socket) {
        Ok(stream) => stream,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Ok(None);
        }
        Err(source) => {
            return Err(CoreError::Io {
                operation: "connect to Tailscale LocalAPI",
                path: socket.to_path_buf(),
                source,
            });
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .and_then(|()| stream.set_write_timeout(Some(Duration::from_secs(2))))
        .map_err(|source| CoreError::Io {
            operation: "configure Tailscale LocalAPI timeout",
            path: socket.to_path_buf(),
            source,
        })?;
    stream
        .write_all(
            b"GET /localapi/v0/status HTTP/1.1\r\nHost: local-tailscaled.sock\r\nSec-Tailscale: localapi\r\nConnection: close\r\n\r\n",
        )
        .map_err(|source| CoreError::Io {
            operation: "query Tailscale LocalAPI",
            path: socket.to_path_buf(),
            source,
        })?;
    let mut response = Vec::new();
    stream
        .take(MAX_TAILSCALE_HTTP_BYTES as u64 + 1)
        .read_to_end(&mut response)
        .map_err(|source| CoreError::Io {
            operation: "read Tailscale LocalAPI",
            path: socket.to_path_buf(),
            source,
        })?;
    if response.len() > MAX_TAILSCALE_HTTP_BYTES {
        return Err(CoreError::ResourceLimit("Tailscale LocalAPI response"));
    }
    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| CoreError::InvalidState("invalid Tailscale LocalAPI response".to_owned()))?;
    if header_end > 64 * 1_024 {
        return Err(CoreError::ResourceLimit("Tailscale LocalAPI headers"));
    }
    let headers = &response[..header_end];
    if !headers.starts_with(b"HTTP/1.1 200 ") && !headers.starts_with(b"HTTP/1.0 200 ") {
        return Ok(None);
    }
    let body = decode_http_body(headers, &response[(header_end + 4)..])?;
    if body.len() > MAX_TAILSCALE_STATUS_BYTES {
        return Err(CoreError::ResourceLimit("Tailscale status"));
    }
    Ok(Some(body))
}

#[cfg(unix)]
fn decode_http_body(headers: &[u8], body: &[u8]) -> Result<Vec<u8>, CoreError> {
    let headers = std::str::from_utf8(headers)
        .map_err(|_| CoreError::InvalidState("invalid Tailscale LocalAPI headers".to_owned()))?;
    let mut content_length = None;
    let mut chunked = false;
    for line in headers.lines().skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            return Err(CoreError::InvalidState(
                "invalid Tailscale LocalAPI header".to_owned(),
            ));
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            let length = value.parse::<usize>().map_err(|_| {
                CoreError::InvalidState("invalid Tailscale content length".to_owned())
            })?;
            if content_length.replace(length).is_some() {
                return Err(CoreError::InvalidState(
                    "duplicate Tailscale content length".to_owned(),
                ));
            }
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            if !value.eq_ignore_ascii_case("chunked") {
                return Err(CoreError::InvalidState(
                    "unsupported Tailscale transfer encoding".to_owned(),
                ));
            }
            chunked = true;
        }
    }
    if chunked && content_length.is_some() {
        return Err(CoreError::InvalidState(
            "ambiguous Tailscale LocalAPI body framing".to_owned(),
        ));
    }
    if chunked {
        return decode_chunked_body(body);
    }
    if let Some(content_length) = content_length
        && (content_length != body.len() || content_length > MAX_TAILSCALE_STATUS_BYTES)
    {
        return Err(CoreError::InvalidState(
            "invalid Tailscale LocalAPI body length".to_owned(),
        ));
    }
    Ok(body.to_vec())
}

#[cfg(unix)]
fn decode_chunked_body(body: &[u8]) -> Result<Vec<u8>, CoreError> {
    let mut cursor = 0_usize;
    let mut decoded = Vec::new();
    loop {
        let line_end = body[cursor..]
            .windows(2)
            .position(|window| window == b"\r\n")
            .map(|offset| cursor + offset)
            .ok_or_else(|| CoreError::InvalidState("invalid Tailscale chunk framing".to_owned()))?;
        let size = std::str::from_utf8(&body[cursor..line_end])
            .ok()
            .and_then(|line| line.split(';').next())
            .and_then(|size| usize::from_str_radix(size.trim(), 16).ok())
            .ok_or_else(|| CoreError::InvalidState("invalid Tailscale chunk size".to_owned()))?;
        cursor = line_end + 2;
        if size == 0 {
            return Ok(decoded);
        }
        if size > MAX_TAILSCALE_STATUS_BYTES.saturating_sub(decoded.len())
            || cursor.saturating_add(size).saturating_add(2) > body.len()
        {
            return Err(CoreError::ResourceLimit("Tailscale status"));
        }
        decoded.extend_from_slice(&body[cursor..cursor + size]);
        cursor += size;
        if body.get(cursor..cursor + 2) != Some(b"\r\n") {
            return Err(CoreError::InvalidState(
                "invalid Tailscale chunk terminator".to_owned(),
            ));
        }
        cursor += 2;
    }
}

#[cfg(not(unix))]
fn tailscale_localapi_status() -> Result<Option<Vec<u8>>, CoreError> {
    Ok(None)
}

fn parse_tailscale_status(
    bytes: &[u8],
    peer_port: u16,
) -> Result<Vec<DiscoveryCandidate>, CoreError> {
    let value: serde_json::Value = serde_json::from_slice(bytes)?;
    let mut candidates = BTreeSet::new();
    if let Some(peers) = value.get("Peer").and_then(serde_json::Value::as_object) {
        for (stable_hint, peer) in peers {
            let service_id = peer
                .get("DNSName")
                .and_then(serde_json::Value::as_str)
                .filter(|name| !name.is_empty())
                .unwrap_or(stable_hint);
            if let Some(addresses) = peer
                .get("TailscaleIPs")
                .and_then(serde_json::Value::as_array)
            {
                for address in addresses {
                    if let Some(address) = address
                        .as_str()
                        .and_then(|value| value.parse::<IpAddr>().ok())
                    {
                        candidates
                            .insert((service_id.to_owned(), SocketAddr::new(address, peer_port)));
                    }
                }
            }
        }
    }
    Ok(candidates
        .into_iter()
        .take(MAX_DISCOVERY_RESULTS)
        .map(|(service_id, endpoint)| DiscoveryCandidate {
            service_id,
            endpoint,
            minimum_protocol_version: PROTOCOL_VERSION,
            maximum_protocol_version: PROTOCOL_VERSION,
            source: DiscoverySource::Tailscale,
        })
        .collect())
}

fn property_u16(info: &mdns_sd::ResolvedService, key: &str) -> Option<u16> {
    info.get_property_val_str(key)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_lan_discovery_has_no_daemon_or_results() {
        let discovery = LanDiscovery::start(false, 4433).expect("disabled discovery");
        assert!(!discovery.is_active());
        assert!(
            LanDiscovery::browse(false, Duration::from_secs(1))
                .expect("disabled browse")
                .is_empty()
        );
    }

    #[test]
    fn tailscale_status_parsing_is_bounded_and_deduplicated() {
        let fixture = br#"{
          "Peer": {
            "node-key:one": {"DNSName":"nas.tail.test.","TailscaleIPs":["100.64.0.2","fd7a:115c:a1e0::2"]},
            "node-key:two": {"DNSName":"mac.tail.test.","TailscaleIPs":["100.64.0.3"]}
          }
        }"#;
        let candidates = parse_tailscale_status(fixture, 4433).expect("parse");
        assert_eq!(candidates.len(), 3);
        assert!(
            candidates
                .iter()
                .all(|candidate| candidate.source == DiscoverySource::Tailscale)
        );
    }

    #[test]
    fn discovery_controller_applies_live_off_on_transitions() {
        let controller = DiscoveryController::new(false, 4433).expect("controller");
        assert!(!controller.is_active().expect("inactive"));
        controller.set_enabled(true).expect("enable discovery");
        assert!(controller.is_active().expect("active"));
        controller.set_enabled(false).expect("disable discovery");
        assert!(!controller.is_active().expect("inactive again"));
    }

    #[cfg(unix)]
    #[test]
    fn tailscale_localapi_socket_is_preferred_and_bounded() {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::net::UnixListener;

        // `sockaddr_un::sun_path` holds 104 bytes on macOS and 108 on Linux, so a
        // socket created under a deeply nested `$TMPDIR` (macOS per-user temp dirs
        // and CI runner temp dirs both nest) cannot be bound at all. Anchor the
        // socket under a short fixed prefix instead of inheriting `$TMPDIR`.
        // `/tmp/covalent-tsXXXXXX/ts.sock` is ~26 bytes, so this always fits on
        // every platform the workspace targets. If it somehow does not, that is a
        // broken environment and the assertion must fail loudly: never silently
        // return, which would report this gate as passing without exercising it.
        const SUN_PATH_CAPACITY: usize = 104;
        let short_base = std::path::Path::new("/tmp");
        let mut builder = tempfile::Builder::new();
        let builder = builder.prefix("covalent-ts");
        let directory = if short_base.is_dir() {
            builder.tempdir_in(short_base)
        } else {
            builder.tempdir()
        }
        .expect("directory");
        let socket = directory.path().join("ts.sock");
        assert!(
            socket.as_os_str().as_bytes().len() < SUN_PATH_CAPACITY,
            "{} needs {} bytes but sun_path holds {}",
            socket.display(),
            socket.as_os_str().as_bytes().len() + 1,
            SUN_PATH_CAPACITY
        );
        let listener = UnixListener::bind(&socket).expect("listen");
        let fixture = br#"{"Peer":{"node-key:one":{"DNSName":"nas.tail.test.","TailscaleIPs":["100.64.0.2"]}}}"#;
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = [0_u8; 512];
            let read = stream.read(&mut request).expect("read request");
            assert!(String::from_utf8_lossy(&request[..read]).contains("Sec-Tailscale: localapi"));
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n",
                fixture.len()
            )
            .expect("write headers");
            stream.write_all(fixture).expect("write body");
            stream.write_all(b"\r\n0\r\n\r\n").expect("finish body");
        });
        let bytes = tailscale_localapi_status_at(&socket)
            .expect("localapi")
            .expect("status body");
        server.join().expect("server");
        let candidates = parse_tailscale_status(&bytes, 4433).expect("parse status");
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].endpoint,
            "100.64.0.2:4433".parse().expect("endpoint")
        );
    }
}
