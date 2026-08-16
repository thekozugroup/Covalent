use std::collections::BTreeSet;
use std::fs;
use std::sync::Arc;

use covalent_core::{
    BackupOptions, ChunkProvider, Engine, EngineOptions, JobControl, RestoreOptions,
};
use covalent_node::transport::{QuicNode, QuicProvider, TlsIdentity};
use covalent_protocol::{BackupId, PeerRole, ReplicaAvailability, ReplicaIntent};
use tempfile::tempdir;

#[tokio::test(flavor = "multi_thread")]
async fn mutually_paired_engines_backup_and_restore_over_pinned_quic() {
    let owner_data = tempdir().expect("owner data");
    let provider_data = tempdir().expect("provider data");
    let source = tempdir().expect("source");
    let restore = tempdir().expect("restore");
    fs::create_dir_all(source.path().join("nested/empty")).expect("source directories");
    let expected: Vec<_> = (0..900_000).map(|index| (index % 239) as u8).collect();
    fs::write(source.path().join("nested/data.bin"), &expected).expect("source file");

    let owner = Arc::new(Engine::open(EngineOptions::new(owner_data.path())).expect("owner"));
    let provider =
        Arc::new(Engine::open(EngineOptions::new(provider_data.path())).expect("provider"));
    let invitation = owner
        .pairing_manager()
        .create_invitation(1_000, 60_000, Vec::new())
        .expect("invitation");
    let mut session = provider
        .accept_pairing(
            invitation,
            "Storage provider",
            BTreeSet::from([PeerRole::StorageProvider]),
            BTreeSet::from([PeerRole::BackupReader, PeerRole::BackupWriter]),
            2_000,
        )
        .expect("accept pairing");
    let code = session.authentication_string().to_string();
    provider
        .confirm_pairing_as_responder(&mut session, &code, 2_000)
        .expect("responder confirmation");
    owner
        .confirm_pairing_as_inviter(&mut session, &code, 2_000)
        .expect("inviter confirmation");
    owner
        .finalize_pairing_as_inviter(&session, 2_000)
        .expect("owner grant");
    provider
        .finalize_pairing_as_responder(&session, 2_000)
        .expect("provider grant");

    let tls = TlsIdentity::load_or_create(provider_data.path().join("tls")).expect("TLS");
    let node = QuicNode::bind(
        "127.0.0.1:0".parse().expect("address"),
        Arc::clone(&provider),
        &tls,
    )
    .expect("QUIC node");
    let address = node.local_addr().expect("QUIC address");
    let node_task = tokio::spawn(node.run());
    let quic_provider = Arc::new(
        QuicProvider::new(
            address,
            provider.public_identity(),
            tls.certificate_der().to_vec(),
            Arc::clone(&owner),
        )
        .expect("QUIC provider"),
    );
    owner
        .set_connected_providers(vec![Arc::clone(&quic_provider) as Arc<dyn ChunkProvider>])
        .expect("connect provider");

    let backup_id = BackupId::new();
    let mut options = BackupOptions::new(backup_id, "0001", "quic-backup");
    options.display_name = "QUIC vertical slice".to_owned();
    options.replica_intent = ReplicaIntent::explicit([provider.device_id()]);
    options.created_at_unix_ms = 3_000;
    let backup = tokio::task::spawn_blocking({
        let owner = Arc::clone(&owner);
        let source_path = source.path().to_path_buf();
        move || owner.backup(source_path, &options, &JobControl::new(), |_| {})
    })
    .await
    .expect("backup worker")
    .expect("backup");
    assert!(
        backup
            .replication
            .is_complete(backup.stored_snapshot.chunk_locators.len())
    );

    let availability = tokio::task::spawn_blocking({
        let owner = Arc::clone(&owner);
        move || owner.verify_snapshot_availability(backup_id, "0001")
    })
    .await
    .expect("verify worker")
    .expect("verify");
    assert_eq!(
        availability.providers.get(&provider.device_id()),
        Some(&ReplicaAvailability::Complete)
    );

    for locator in &backup.stored_snapshot.chunk_locators {
        fs::remove_file(
            owner
                .store()
                .root()
                .join("chunks")
                .join(&locator[..2])
                .join(&locator[2..]),
        )
        .expect("remove local chunk");
    }
    let source_path = source.path().to_path_buf();
    drop(source);
    assert!(!source_path.exists());
    let plan = owner
        .preview_restore(
            backup_id,
            "0001",
            restore.path(),
            &RestoreOptions::all("quic-restore"),
        )
        .expect("preview");
    tokio::task::spawn_blocking({
        let owner = Arc::clone(&owner);
        move || owner.restore(&plan, &JobControl::new())
    })
    .await
    .expect("restore worker")
    .expect("restore");
    assert_eq!(
        fs::read(restore.path().join("nested/data.bin")).expect("restored file"),
        expected
    );
    assert!(restore.path().join("nested/empty").is_dir());

    owner
        .revoke_peer(provider.device_id())
        .expect("revoke provider");
    let revoked = owner
        .verify_snapshot_availability(backup_id, "0001")
        .expect("revoked availability");
    assert_eq!(
        revoked.providers.get(&provider.device_id()),
        Some(&ReplicaAvailability::Revoked)
    );
    node_task.abort();
}
