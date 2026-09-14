use super::config::EngineDeviceId;
use super::{EnginePeerConnection, EnginePeerConnectionState};

#[test]
fn connection_debug_redacts_peer_identity() {
    let id =
        EngineDeviceId::parse("EA2I6UL-UUWKJDX-NGOC6QS-U2IZR4Z-LXY5CB7-3KBVQRG-4CNFBV7-RACTXQF")
            .unwrap();
    let connection =
        EnginePeerConnection::new_runtime(id.clone(), EnginePeerConnectionState::Connected);
    assert_eq!(connection.id(), &id);
    assert_eq!(connection.state(), EnginePeerConnectionState::Connected);
    let debug = format!("{connection:?}");
    assert!(!debug.contains(id.as_str()));
    assert!(debug.contains("[REDACTED]"));
}
