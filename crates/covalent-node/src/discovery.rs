//! Opt-in minimal LAN advertisements and untrusted Tailnet candidate discovery.

use std::collections::BTreeSet;
use std::net::{IpAddr, SocketAddr};
use std::process::Command;
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

/// Reads bounded Tailscale CLI status as routing hints. No Tailscale identity is trusted.
pub fn discover_tailscale_candidates(peer_port: u16) -> Result<Vec<DiscoveryCandidate>, CoreError> {
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
}
