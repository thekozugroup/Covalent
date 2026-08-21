use std::collections::BTreeSet;

use covalent_core::{BackupKey, Engine, EngineOptions, ProviderQuotaPolicy};
use covalent_protocol::{BackupId, PeerGrant, PeerRole};
use tempfile::tempdir;

fn writer_grant(writer: &Engine) -> PeerGrant {
    PeerGrant {
        peer_device_id: writer.device_id(),
        public_key: writer.public_identity().public_key,
        display_name: "Backup writer".to_owned(),
        roles: BTreeSet::from([PeerRole::BackupWriter, PeerRole::BackupReader]),
        confirmed_at_unix_ms: 1,
        revoked: false,
    }
}

fn small_provider_options(path: &std::path::Path) -> EngineOptions {
    let mut options = EngineOptions::new(path);
    options.provider_quota_policy = ProviderQuotaPolicy {
        maximum_total_bytes: 2_048,
        maximum_peer_bytes: 2_048,
        maximum_backup_bytes: 1_024,
        maximum_total_objects: 8,
        maximum_peer_objects: 8,
        maximum_backup_objects: 4,
        free_space_reserve_bytes: 0,
        maximum_lease_lifetime_ms: 10_000,
    };
    options
}

#[test]
fn signed_lease_rejects_cross_scope_expiry_overage_and_survives_restart() {
    let directory = tempdir().expect("directory");
    let writer = Engine::open(EngineOptions::new(directory.path().join("writer"))).expect("writer");
    let provider_path = directory.path().join("provider");
    let provider = Engine::open(small_provider_options(&provider_path)).expect("provider");
    provider
        .trust_peer(writer_grant(&writer))
        .expect("trust writer");

    let backup_id = BackupId::new();
    let encrypted = BackupKey::generate()
        .encrypt_chunk(backup_id, 1, b"leased bytes")
        .expect("encrypted chunk");
    let record = encrypted.encode_provider_record();
    let lease = provider
        .issue_storage_lease(
            writer.device_id(),
            backup_id,
            record.len() as u64,
            1,
            100,
            1_000,
        )
        .expect("lease");
    assert!(
        provider
            .put_leased_provider_record(
                writer.device_id(),
                &lease,
                &encrypted.opaque_locator,
                &record,
                200,
            )
            .expect("first write")
    );
    assert!(
        !provider
            .put_leased_provider_record(
                writer.device_id(),
                &lease,
                &encrypted.opaque_locator,
                &record,
                300,
            )
            .expect("idempotent replay")
    );

    let mut wrong_backup = lease.clone();
    wrong_backup.backup_id = BackupId::new();
    assert!(
        provider
            .put_leased_provider_record(
                writer.device_id(),
                &wrong_backup,
                &encrypted.opaque_locator,
                &record,
                300,
            )
            .is_err()
    );
    assert!(
        provider
            .put_leased_provider_record(
                covalent_protocol::DeviceId::new(),
                &lease,
                &encrypted.opaque_locator,
                &record,
                300,
            )
            .is_err()
    );
    assert!(
        provider
            .put_leased_provider_record(
                writer.device_id(),
                &lease,
                &encrypted.opaque_locator,
                &record,
                1_000,
            )
            .is_err()
    );

    let over_backup = BackupId::new();
    let over_chunk = BackupKey::generate()
        .encrypt_chunk(over_backup, 1, b"different leased bytes")
        .expect("over-limit encrypted chunk");
    let over_record = over_chunk.encode_provider_record();
    let over_limit = provider
        .issue_storage_lease(
            writer.device_id(),
            over_backup,
            (over_record.len() - 1) as u64,
            1,
            100,
            1_000,
        )
        .expect("small lease");
    assert!(
        provider
            .put_leased_provider_record(
                writer.device_id(),
                &over_limit,
                &over_chunk.opaque_locator,
                &over_record,
                200,
            )
            .is_err()
    );

    let reserved_backup = BackupId::new();
    provider
        .issue_storage_lease(writer.device_id(), reserved_backup, 900, 1, 2_000, 3_000)
        .expect("first reservation");
    assert!(
        provider
            .issue_storage_lease(writer.device_id(), reserved_backup, 200, 1, 2_000, 3_000)
            .is_err()
    );

    drop(provider);
    let reopened = Engine::open(small_provider_options(&provider_path)).expect("restart provider");
    assert!(
        !reopened
            .put_leased_provider_record(
                writer.device_id(),
                &lease,
                &encrypted.opaque_locator,
                &record,
                400,
            )
            .expect("durable lease replay after restart")
    );
}
