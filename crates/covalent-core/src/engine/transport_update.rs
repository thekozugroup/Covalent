use std::net::{IpAddr, SocketAddr};

use super::*;

impl Engine {
    /// Durably changes only the address of an already trusted transport.
    ///
    /// The caller must first authenticate a fresh response at the candidate
    /// address using the retained certificate and peer signing identity. It
    /// must also prepare any dependent folder-journal transaction and stop its
    /// worker before this call. Discovery alone never authorizes this update.
    ///
    /// Both the complete grant and transport are compared under the config
    /// lock. Identity, certificate, name, roles and confirmation time cannot
    /// change through this operation. Returns false for an exact completed
    /// retry, without issuing another roster epoch. A pending config commit
    /// requires cold recovery before any update or retry can succeed.
    pub fn refresh_trusted_peer_address(
        &self,
        expected_grant: &PeerGrant,
        expected_transport: &TransportBinding,
        candidate_address: &str,
    ) -> Result<bool, CoreError> {
        validate_candidate_address(candidate_address)?;
        let identity = PublicIdentity::from_encoded(
            expected_grant.peer_device_id,
            expected_grant.public_key.clone(),
        )?;
        crate::pairing::validate_transport_binding(
            expected_transport,
            &identity,
            &expected_grant.display_name,
        )?;
        let mut replacement = expected_transport.clone();
        replacement.address = candidate_address.to_owned();
        let mut config = self.config.lock().map_err(|_| CoreError::Synchronization)?;
        let grant = config
            .trusted_peers
            .get(&expected_grant.peer_device_id)
            .ok_or(CoreError::IdentityMismatch)?;
        if grant.revoked {
            return Err(CoreError::PeerRevoked);
        }
        if grant != expected_grant || grant.confirmed_at_unix_ms == 0 {
            return Err(CoreError::IdentityMismatch);
        }
        let transaction_path = roster_transaction_path(&self.options.data_directory);
        match fs::symlink_metadata(&transaction_path) {
            Ok(_) => {
                return Err(CoreError::InvalidState(
                    "pending roster transaction requires recovery".to_owned(),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(CoreError::Io {
                    operation: "inspect transport update transaction",
                    path: transaction_path,
                    source,
                });
            }
        }
        let current = config
            .trusted_peer_transports
            .get(&expected_grant.peer_device_id)
            .ok_or(CoreError::IdentityMismatch)?;
        if current == &replacement {
            return Ok(false);
        }
        if current != expected_transport {
            return Err(CoreError::IdentityMismatch);
        }
        let mut candidate = config.clone();
        candidate
            .trusted_peer_transports
            .insert(expected_grant.peer_device_id, replacement);
        self.issue_roster_locked(&config, &mut candidate)?;
        *config = candidate;
        Ok(true)
    }
}

fn validate_candidate_address(value: &str) -> Result<(), CoreError> {
    let address = (value.len() <= 128)
        .then(|| value.parse::<SocketAddr>().ok())
        .flatten()
        .ok_or_else(|| CoreError::InvalidState("invalid peer candidate address".to_owned()))?;
    let ambiguous_ipv6 = match address {
        SocketAddr::V6(address) => {
            address.ip().segments()[0] & 0xffc0 == 0xfe80
                || address.scope_id() != 0
                || address.flowinfo() != 0
        }
        SocketAddr::V4(_) => false,
    };
    if address.port() == 0
        || address.ip().is_unspecified()
        || address.ip().is_multicast()
        || matches!(address.ip(), IpAddr::V4(ip) if ip.is_broadcast())
        || ambiguous_ipv6
        || address.to_string() != value
    {
        return Err(CoreError::InvalidState(
            "peer candidate address must be canonical unicast".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};
    use tempfile::{TempDir, tempdir};

    use super::*;

    fn options(path: &Path) -> EngineOptions {
        EngineOptions::new(path).with_key_protector(Arc::new(
            crate::StaticKeyProtector::new(1, [0x63; 32]).expect("test protector"),
        ))
    }

    fn fixture() -> (TempDir, Engine, PeerGrant, TransportBinding) {
        let directory = tempdir().expect("owned directory");
        let engine = Engine::open(options(directory.path())).expect("engine");
        let peer = DeviceIdentity::generate().public_identity();
        let grant = PeerGrant {
            peer_device_id: peer.device_id,
            public_key: peer.public_key,
            display_name: "Confirmed peer".to_owned(),
            roles: BTreeSet::from([PeerRole::BackupReader, PeerRole::StorageProvider]),
            confirmed_at_unix_ms: 17,
            revoked: false,
        };
        // Core pairing validates retained bytes/digest. Actual certificate and
        // signature authentication belongs to the separate network exchange.
        let certificate = b"unit fixture certificate identity";
        let transport = TransportBinding {
            peer_id: grant.peer_device_id,
            display_name: grant.display_name.clone(),
            address: "192.0.2.10:22000".to_owned(),
            certificate_der: URL_SAFE_NO_PAD.encode(certificate),
            certificate_fingerprint: format!("{:x}", Sha256::digest(certificate)),
        };
        engine
            .trust_peer_with_transport(grant.clone(), Some(transport.clone()))
            .expect("retained trust");
        (directory, engine, grant, transport)
    }

    #[test]
    fn address_update_preserves_authority_and_exact_retry_across_reopen() {
        let (directory, engine, grant, transport) = fixture();
        let before = engine.config().expect("before");
        let local_identity = engine.public_identity();
        let address = "[2001:db8::20]:23000";
        assert!(
            engine
                .refresh_trusted_peer_address(&grant, &transport, address)
                .expect("update")
        );
        let after = engine.config().expect("after");
        let mut expected = before.clone();
        expected
            .trusted_peer_transports
            .get_mut(&grant.peer_device_id)
            .expect("pin")
            .address = address.to_owned();
        expected.roster_epoch = after.roster_epoch;
        expected.roster_digest = after.roster_digest.clone();
        assert_eq!(after, expected);
        assert_eq!(after.roster_epoch, before.roster_epoch + 1);
        assert!(
            !engine
                .refresh_trusted_peer_address(&grant, &transport, address)
                .expect("retry")
        );
        assert_eq!(engine.config().expect("unchanged"), after);
        drop(engine);
        let reopened = Engine::open(options(directory.path())).expect("reopen");
        assert_eq!(reopened.public_identity(), local_identity);
        assert_eq!(reopened.config().expect("durable update"), after);
        assert!(
            !reopened
                .refresh_trusted_peer_address(&grant, &transport, address)
                .expect("cold retry")
        );
        assert_eq!(reopened.config().expect("same epoch"), after);
    }

    #[test]
    fn address_update_rejects_invalid_candidates_without_durable_changes() {
        let (_directory, engine, grant, transport) = fixture();
        let before = engine.config().expect("before");
        for address in [
            "",
            "peer.local:22000",
            "https://192.0.2.20:22000",
            "192.0.2.20:0",
            "192.0.2.20:022000",
            "0.0.0.0:22000",
            "[::]:22000",
            "224.0.0.1:22000",
            "[ff02::1]:22000",
            "[fe80::1]:22000",
            "[fe80::1%2]:22000",
            "255.255.255.255:22000",
            "192.0.2.20:22000 ",
        ] {
            assert!(
                engine
                    .refresh_trusted_peer_address(&grant, &transport, address)
                    .is_err(),
                "{address}"
            );
            assert_eq!(engine.config().expect("unchanged"), before);
        }
        assert!(
            engine
                .refresh_trusted_peer_address(&grant, &transport, &"a".repeat(129))
                .is_err()
        );
        assert!(
            !engine
                .refresh_trusted_peer_address(&grant, &transport, &transport.address)
                .expect("already current")
        );
        assert_eq!(engine.config().expect("unchanged"), before);
    }

    #[test]
    fn address_update_rejects_changed_grants_pins_revocation_and_stale_address() {
        let (_directory, engine, grant, transport) = fixture();
        let before = engine.config().expect("before");
        let mut other_grant = grant.clone();
        other_grant.confirmed_at_unix_ms += 1;
        assert!(
            engine
                .refresh_trusted_peer_address(&other_grant, &transport, "192.0.2.20:22000")
                .is_err()
        );
        let mut other_pin = transport.clone();
        other_pin.certificate_der = URL_SAFE_NO_PAD.encode(b"different pin");
        other_pin.certificate_fingerprint = format!("{:x}", Sha256::digest(b"different pin"));
        assert!(
            engine
                .refresh_trusted_peer_address(&grant, &other_pin, "192.0.2.20:22000")
                .is_err()
        );
        other_pin = transport.clone();
        other_pin.display_name = "Unconfirmed name".to_owned();
        assert!(
            engine
                .refresh_trusted_peer_address(&grant, &other_pin, "192.0.2.20:22000")
                .is_err()
        );
        assert_eq!(engine.config().expect("rejections preserve state"), before);
        assert!(
            engine
                .refresh_trusted_peer_address(&grant, &transport, "192.0.2.20:22000")
                .expect("first move")
        );
        let moved = engine.config().expect("moved");
        assert!(
            engine
                .refresh_trusted_peer_address(&grant, &transport, "192.0.2.30:22000")
                .is_err()
        );
        assert_eq!(engine.config().expect("stale rejected"), moved);
        engine.revoke_peer(grant.peer_device_id).expect("revoke");
        let revoked = engine.config().expect("revoked");
        assert!(matches!(
            engine.refresh_trusted_peer_address(&grant, &transport, "192.0.2.20:22000"),
            Err(CoreError::PeerRevoked)
        ));
        assert_eq!(engine.config().expect("not resurrected"), revoked);
    }

    #[test]
    fn interrupted_address_commit_recovers_before_a_retry_is_allowed() {
        let (directory, engine, grant, transport) = fixture();
        let before = engine.config().expect("before");
        let roster = directory.path().join("roster.json");
        let saved_roster = directory.path().join("saved-roster.json");
        fs::rename(&roster, &saved_roster).expect("hold owned roster");
        fs::create_dir(&roster).expect("inject publication failure");
        let address = "192.0.2.20:22000";
        assert!(
            engine
                .refresh_trusted_peer_address(&grant, &transport, address)
                .is_err()
        );
        assert_eq!(engine.config().expect("unpublished in memory"), before);
        assert!(roster_transaction_path(directory.path()).is_file());
        assert!(
            engine
                .refresh_trusted_peer_address(&grant, &transport, address)
                .is_err()
        );
        fs::remove_dir(&roster).expect("remove owned obstruction");
        fs::rename(saved_roster, roster).expect("restore owned roster");
        drop(engine);
        let reopened =
            Engine::open(options(directory.path())).expect("recover pending transaction");
        let recovered = reopened.config().expect("recovered");
        assert_eq!(recovered.trusted_peers, before.trusted_peers);
        assert_eq!(
            recovered.trusted_peer_transports[&grant.peer_device_id].address,
            address
        );
        assert_eq!(recovered.roster_epoch, before.roster_epoch + 1);
        assert!(!roster_transaction_path(directory.path()).exists());
        assert!(
            !reopened
                .refresh_trusted_peer_address(&grant, &transport, address)
                .expect("exact retry")
        );
    }

    #[test]
    fn concurrent_address_candidates_cannot_overwrite_each_other() {
        let (_directory, engine, grant, transport) = fixture();
        let engine = Arc::new(engine);
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = ["192.0.2.20:22000", "192.0.2.30:22000"]
            .into_iter()
            .map(|address| {
                let engine = engine.clone();
                let barrier = barrier.clone();
                let grant = grant.clone();
                let transport = transport.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    engine.refresh_trusted_peer_address(&grant, &transport, address)
                })
            })
            .collect();
        let results: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().expect("thread"))
            .collect();
        assert_eq!(
            results
                .iter()
                .filter(|value| matches!(value, Ok(true)))
                .count(),
            1
        );
        assert_eq!(results.iter().filter(|value| value.is_err()).count(), 1);
        assert_eq!(engine.config().expect("one update").roster_epoch, 2);
    }
}
