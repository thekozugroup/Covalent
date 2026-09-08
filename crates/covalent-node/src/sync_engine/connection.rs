//! Bounded, redacted observations of configured engine peer connections.
//!
//! The pinned engine includes addresses, client versions and transfer counters
//! in this response. This decoder deliberately retains only the configured
//! engine identity and the two booleans needed for connection state.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::Deserialize;
use serde::de::{Error as _, MapAccess, Visitor};

use super::config::EngineDeviceId;
use super::{EngineApiClient, EngineApiError, EngineEndpoint};

const MAX_PEERS: usize = 128;

/// One fresh observation for an exact configured engine peer.
#[derive(Clone, Eq, PartialEq)]
pub struct EnginePeerConnection {
    id: EngineDeviceId,
    state: EnginePeerConnectionState,
}

impl fmt::Debug for EnginePeerConnection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EnginePeerConnection")
            .field("id", &"[REDACTED]")
            .field("state", &self.state)
            .finish()
    }
}

impl EnginePeerConnection {
    #[cfg(test)]
    pub(crate) fn new(id: EngineDeviceId, state: EnginePeerConnectionState) -> Self {
        Self { id, state }
    }

    #[must_use]
    pub fn id(&self) -> &EngineDeviceId {
        &self.id
    }

    #[must_use]
    pub const fn state(&self) -> EnginePeerConnectionState {
        self.state
    }
}

/// State reported by the pinned engine for one configured peer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnginePeerConnectionState {
    Connected,
    Disconnected,
    Paused,
}

/// Fixed failure from the bounded connection observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PeerConnectionError {
    InvalidInput,
    Unavailable,
    InvalidResponse,
}

impl fmt::Display for PeerConnectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidInput => "invalid peer connection request",
            Self::Unavailable => "peer connection status is unavailable",
            Self::InvalidResponse => "peer connection status is invalid",
        })
    }
}

impl std::error::Error for PeerConnectionError {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConnectionResponse {
    connections: Connections,
    #[serde(rename = "total")]
    _total: serde::de::IgnoredAny,
}

#[derive(Deserialize)]
struct ConnectionEntry {
    connected: bool,
    paused: bool,
}

struct Connections(BTreeMap<String, ConnectionEntry>);

impl<'de> Deserialize<'de> for Connections {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct ConnectionsVisitor;
        impl<'de> Visitor<'de> for ConnectionsVisitor {
            type Value = Connections;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a bounded map of configured peer connections")
            }

            fn visit_map<A>(self, mut access: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut result = BTreeMap::new();
                while let Some((id, entry)) = access.next_entry::<String, ConnectionEntry>()? {
                    if result.len() == MAX_PEERS || result.insert(id, entry).is_some() {
                        return Err(A::Error::custom("invalid peer connection map"));
                    }
                }
                Ok(Connections(result))
            }
        }
        deserializer.deserialize_map(ConnectionsVisitor)
    }
}

/// Read connection booleans for exactly the controller-configured peers.
///
/// Missing, additional, duplicated or noncanonical identities reject the
/// entire observation. A caller can therefore degrade every mapped peer to
/// unknown instead of accidentally retaining a stale connected claim.
pub async fn collect_peer_connections(
    client: &EngineApiClient,
    peers: &[EngineDeviceId],
) -> Result<Vec<EnginePeerConnection>, PeerConnectionError> {
    if peers.is_empty()
        || peers.len() > MAX_PEERS
        || peers
            .iter()
            .map(EngineDeviceId::as_str)
            .collect::<BTreeSet<_>>()
            .len()
            != peers.len()
    {
        return Err(PeerConnectionError::InvalidInput);
    }
    let response: ConnectionResponse = client
        .json(EngineEndpoint::Connections)
        .await
        .map_err(map_client_error)?;
    if response.connections.0.len() != peers.len() {
        return Err(PeerConnectionError::InvalidResponse);
    }
    let mut remaining = response.connections.0;
    let mut result = Vec::new();
    result
        .try_reserve_exact(peers.len())
        .map_err(|_| PeerConnectionError::InvalidResponse)?;
    for peer in peers {
        let entry = remaining
            .remove(peer.as_str())
            .ok_or(PeerConnectionError::InvalidResponse)?;
        result.push(EnginePeerConnection {
            id: peer.clone(),
            state: if entry.paused {
                EnginePeerConnectionState::Paused
            } else if entry.connected {
                EnginePeerConnectionState::Connected
            } else {
                EnginePeerConnectionState::Disconnected
            },
        });
    }
    if !remaining.is_empty() {
        return Err(PeerConnectionError::InvalidResponse);
    }
    Ok(result)
}

fn map_client_error(error: EngineApiError) -> PeerConnectionError {
    match error {
        EngineApiError::InvalidResponse | EngineApiError::BodyTooLarge => {
            PeerConnectionError::InvalidResponse
        }
        _ => PeerConnectionError::Unavailable,
    }
}
