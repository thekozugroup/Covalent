//! Redacted connection state reported by the owned rclone runtime.

use std::fmt;

use super::config::EngineDeviceId;

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
    pub(crate) fn new_runtime(id: EngineDeviceId, state: EnginePeerConnectionState) -> Self {
        Self { id, state }
    }

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

/// State observed by the runtime for one configured peer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnginePeerConnectionState {
    Connected,
    Disconnected,
    Paused,
}
