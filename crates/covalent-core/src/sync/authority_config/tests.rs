use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::Path;

use ed25519_dalek::SigningKey;

use super::*;
use crate::StaticKeyProtector;
use crate::sync::event::{EventEnvelope, EventKind};
use crate::sync::event_log::{EventMachine as _, PreparedEvent};
use crate::sync::installation::SyncInstallation;
use crate::sync::machine::FolderEventMachine;
use crate::sync::membership::encode_signed_epoch;
use crate::sync::state_dir::PrivateStateDir;

fn folder() -> FolderId {
    FolderId::from_uuid(Uuid::from_u128(940))
}
fn transport() -> (DeviceId, VerifyingKey) {
    (
        DeviceId::from_uuid(Uuid::from_u128(941)),
        SigningKey::from_bytes(&[94; 32]).verifying_key(),
    )
}
fn protector() -> StaticKeyProtector {
    StaticKeyProtector::new(1, [95; 32]).unwrap()
}
fn directory() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    root
}
fn install(path: &Path, create: bool) -> SyncInstallationParts {
    let root = PrivateStateDir::open_root(path).unwrap();
    let installation = if create {
        SyncInstallation::create(root, folder(), &protector())
    } else {
        SyncInstallation::open(root, folder(), &protector())
    };
    installation.unwrap().into_parts().unwrap()
}
fn local_pin(parts: &SyncInstallationParts) -> PinnedFolderAuthority {
    PinnedFolderAuthority::new(parts.writer_id(), parts.signing_key().verifying_key()).unwrap()
}
fn create(parts: &SyncInstallationParts) -> FolderAuthorityConfig {
    let (device, key) = transport();
    FolderAuthorityConfig::create(parts, local_pin(parts), device, key, &protector()).unwrap()
}
fn limits() -> FolderMachineLimits {
    FolderMachineLimits {
        maximum_index_bytes: 1 << 20,
        maximum_operations: 100,
        maximum_paths: 100,
        maximum_membership_epochs: 16,
        maximum_pending_evidence_bytes: 1 << 18,
        maximum_pending_evidence_records: 128,
    }
}

#[test]
fn protected_pins_survive_restart_and_drive_genesis_admission() {
    let root = directory();
    let parts = install(root.path(), true);
    let config = create(&parts);
    let original = std::fs::read(root.path().join(RECORD_KEY)).unwrap();
    let pin = config.authority();
    let setup_binding = config.setup_binding();
    let (device, key) = transport();
    config.require_local_transport(device, key).unwrap();
    let grant = MemberGrant::new(
        parts.writer_id(),
        parts.signing_key().verifying_key(),
        device,
        key,
        MemberRole::ReadWrite,
    )
    .unwrap();
    let epoch = encode_signed_epoch(
        parts.signing_key(),
        folder(),
        1,
        parts.writer_id(),
        None,
        &[grant],
        None,
        &[],
        &[],
        &[],
        &[],
    )
    .unwrap();
    let event = EventEnvelope::from_signed_record(EventKind::MembershipEpoch, &epoch)
        .unwrap()
        .encode()
        .unwrap();
    let mut machine = FolderEventMachine::new(config.machine_config(limits())).unwrap();
    let PreparedEvent::Append(prepared) = machine.prepare_event(event.as_bytes()).unwrap() else {
        panic!("fresh genesis must append")
    };
    machine.commit(prepared).unwrap();
    assert_eq!(machine.current_head().unwrap().epoch(), 1);
    drop(parts);
    let reopened = install(root.path(), false);
    let restored = FolderAuthorityConfig::open(&reopened, &protector()).unwrap();
    assert_eq!(restored.authority(), pin);
    assert_eq!(restored.setup_binding(), setup_binding);
    assert_eq!(
        std::fs::read(root.path().join(RECORD_KEY)).unwrap(),
        original
    );
    restored.require_local_transport(device, key).unwrap();
    assert!(matches!(
        restored.require_local_transport(DeviceId::from_uuid(Uuid::from_u128(942)), key),
        Err(AuthorityConfigError::WrongTransport)
    ));
    assert!(matches!(
        restored.require_local_transport(device, SigningKey::from_bytes(&[96; 32]).verifying_key()),
        Err(AuthorityConfigError::WrongTransport)
    ));
    assert_eq!(
        std::fs::metadata(root.path().join(RECORD_KEY))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(matches!(
        PrivateStateDir::open_root(root.path()).unwrap().try_lock(),
        Err(StateDirError::Locked)
    ));
}

#[test]
fn separately_pinned_remote_authority_rejects_local_self_appointment() {
    let root = directory();
    let parts = install(root.path(), true);
    let remote = PinnedFolderAuthority::new(
        WriterId::from_uuid(Uuid::from_u128(945)),
        SigningKey::from_bytes(&[99; 32]).verifying_key(),
    )
    .unwrap();
    let (device, key) = transport();
    let config = FolderAuthorityConfig::create(&parts, remote, device, key, &protector()).unwrap();
    assert_eq!(config.authority(), remote);
    let grant = MemberGrant::new(
        parts.writer_id(),
        parts.signing_key().verifying_key(),
        device,
        key,
        MemberRole::ReadWrite,
    )
    .unwrap();
    let epoch = encode_signed_epoch(
        parts.signing_key(),
        folder(),
        1,
        parts.writer_id(),
        None,
        &[grant],
        None,
        &[],
        &[],
        &[],
        &[],
    )
    .unwrap();
    let event = EventEnvelope::from_signed_record(EventKind::MembershipEpoch, &epoch)
        .unwrap()
        .encode()
        .unwrap();
    let machine = FolderEventMachine::new(config.machine_config(limits())).unwrap();
    assert!(machine.prepare_event(event.as_bytes()).is_err());
}

#[test]
fn remote_authority_genesis_is_accepted_before_and_after_pins_reopen() {
    let root = directory();
    let parts = install(root.path(), true);
    let local_writer = parts.writer_id();
    let remote_key = SigningKey::from_bytes(&[104; 32]);
    let remote_writer = WriterId::from_uuid(Uuid::from_u128(951));
    let pin = PinnedFolderAuthority::new(remote_writer, remote_key.verifying_key()).unwrap();
    let (device, key) = transport();
    let config = FolderAuthorityConfig::create(&parts, pin, device, key, &protector()).unwrap();
    let authority_grant = MemberGrant::new(
        remote_writer,
        remote_key.verifying_key(),
        DeviceId::from_uuid(Uuid::from_u128(952)),
        SigningKey::from_bytes(&[105; 32]).verifying_key(),
        MemberRole::ReadWrite,
    )
    .unwrap();
    // Genesis contains only the authority. The local joining writer remains
    // unadmitted until the separate bootstrap-backed membership transition.
    let epoch = encode_signed_epoch(
        &remote_key,
        folder(),
        1,
        remote_writer,
        None,
        &[authority_grant],
        None,
        &[],
        &[],
        &[],
        &[],
    )
    .unwrap();
    let event = EventEnvelope::from_signed_record(EventKind::MembershipEpoch, &epoch)
        .unwrap()
        .encode()
        .unwrap();
    assert_eq!(
        config.machine_config(limits()).local_writer_id,
        local_writer
    );
    let mut machine = FolderEventMachine::new(config.machine_config(limits())).unwrap();
    let PreparedEvent::Append(prepared) = machine.prepare_event(event.as_bytes()).unwrap() else {
        panic!("fresh genesis must append")
    };
    machine.commit(prepared).unwrap();
    assert_eq!(machine.current_head().unwrap().epoch(), 1);
    drop(parts);
    let reopened = install(root.path(), false);
    let restored = FolderAuthorityConfig::open(&reopened, &protector()).unwrap();
    assert_eq!(restored.authority(), pin);
    assert_eq!(
        restored.machine_config(limits()).local_writer_id,
        local_writer
    );
    let mut machine = FolderEventMachine::new(restored.machine_config(limits())).unwrap();
    let PreparedEvent::Append(prepared) = machine.prepare_event(event.as_bytes()).unwrap() else {
        panic!("fresh genesis must append")
    };
    machine.commit(prepared).unwrap();
    assert_eq!(
        machine.current_head().unwrap().roster()[0].writer_id(),
        remote_writer
    );
}

#[test]
fn immutable_incumbents_and_existing_child_state_never_repin() {
    let root = directory();
    let parts = install(root.path(), true);
    create(&parts);
    let original = std::fs::read(root.path().join(RECORD_KEY)).unwrap();
    let (device, key) = transport();
    assert!(
        FolderAuthorityConfig::create(&parts, local_pin(&parts), device, key, &protector())
            .is_err()
    );
    assert_eq!(
        std::fs::read(root.path().join(RECORD_KEY)).unwrap(),
        original
    );
    std::fs::write(root.path().join(RECORD_KEY), b"partial").unwrap();
    assert!(FolderAuthorityConfig::open(&parts, &protector()).is_err());
    assert!(
        FolderAuthorityConfig::create(&parts, local_pin(&parts), device, key, &protector())
            .is_err()
    );
    assert_eq!(
        std::fs::read(root.path().join(RECORD_KEY)).unwrap(),
        b"partial"
    );

    let other = directory();
    let other_parts = install(other.path(), true);
    other_parts
        .directory
        .open_or_create_child(&StateKey::new("events").unwrap())
        .unwrap();
    assert!(matches!(
        FolderAuthorityConfig::create(
            &other_parts,
            local_pin(&other_parts),
            device,
            key,
            &protector()
        ),
        Err(AuthorityConfigError::DirectoryNotFresh)
    ));
    assert!(!other.path().join(RECORD_KEY).exists());
    assert!(FolderAuthorityConfig::open(&other_parts, &protector()).is_err());

    let initialized = directory();
    let initialized_parts = install(initialized.path(), true);
    create(&initialized_parts);
    initialized_parts
        .directory
        .open_or_create_child(&StateKey::new("events").unwrap())
        .unwrap();
    std::fs::remove_file(initialized.path().join(RECORD_KEY)).unwrap();
    assert!(FolderAuthorityConfig::open(&initialized_parts, &protector()).is_err());
    assert!(matches!(
        FolderAuthorityConfig::create(
            &initialized_parts,
            local_pin(&initialized_parts),
            device,
            key,
            &protector()
        ),
        Err(AuthorityConfigError::DirectoryNotFresh)
    ));
    assert!(!initialized.path().join(RECORD_KEY).exists());
}

#[test]
fn authenticated_context_rejects_other_installation_generation_folder_writer_and_key() {
    let root = directory();
    let mut parts = install(root.path(), true);
    create(&parts);
    let bytes = std::fs::read(root.path().join(RECORD_KEY)).unwrap();
    let bad_protector = StaticKeyProtector::new(1, [101; 32]).unwrap();
    assert!(matches!(
        decode(&parts, &bytes, &bad_protector),
        Err(AuthorityConfigError::Authentication)
    ));
    let original = parts.folder_id;
    parts.folder_id = FolderId::from_uuid(Uuid::from_u128(946));
    assert!(matches!(
        decode(&parts, &bytes, &protector()),
        Err(AuthorityConfigError::Authentication)
    ));
    parts.folder_id = original;
    let original = parts.installation_id;
    parts.installation_id = Uuid::from_u128(947);
    assert!(matches!(
        decode(&parts, &bytes, &protector()),
        Err(AuthorityConfigError::Authentication)
    ));
    parts.installation_id = original;
    let original = parts.generation_id;
    parts.generation_id = Uuid::from_u128(948);
    assert!(matches!(
        decode(&parts, &bytes, &protector()),
        Err(AuthorityConfigError::Authentication)
    ));
    parts.generation_id = original;
    let original = parts.writer_id;
    parts.writer_id = WriterId::from_uuid(Uuid::from_u128(949));
    assert!(matches!(
        decode(&parts, &bytes, &protector()),
        Err(AuthorityConfigError::Authentication)
    ));
    parts.writer_id = original;
    parts.signing_key = SigningKey::from_bytes(&[102; 32]);
    assert!(matches!(
        decode(&parts, &bytes, &protector()),
        Err(AuthorityConfigError::Authentication)
    ));
    assert_eq!(std::fs::read(root.path().join(RECORD_KEY)).unwrap(), bytes);
}

#[test]
fn malformed_noncanonical_tampered_and_oversized_records_fail_without_repair() {
    let root = directory();
    let parts = install(root.path(), true);
    create(&parts);
    let original = std::fs::read(root.path().join(RECORD_KEY)).unwrap();
    for end in 0..original.len() {
        assert!(
            decode(&parts, &original[..end], &protector()).is_err(),
            "truncation {end}"
        );
    }
    for index in [0, 8, 9, 10, 11, 14, HEADER_BYTES, original.len() - 2] {
        let mut bytes = original.clone();
        bytes[index] ^= 1;
        assert!(
            decode(&parts, &bytes, &protector()).is_err(),
            "mutation {index}"
        );
    }
    let mut bytes = original.clone();
    bytes.push(b' ');
    let length = (bytes.len() - HEADER_BYTES) as u32;
    bytes[11..15].copy_from_slice(&length.to_be_bytes());
    assert!(matches!(
        decode(&parts, &bytes, &protector()),
        Err(AuthorityConfigError::InvalidRecord)
    ));
    assert!(
        decode(
            &parts,
            &vec![0; MAX_AUTHORITY_RECORD_BYTES + 1],
            &protector()
        )
        .is_err()
    );
    std::fs::write(root.path().join(RECORD_KEY), &bytes).unwrap();
    assert!(FolderAuthorityConfig::open(&parts, &protector()).is_err());
    assert_eq!(std::fs::read(root.path().join(RECORD_KEY)).unwrap(), bytes);
}

#[test]
fn structurally_invalid_or_reused_keys_are_rejected_before_writing() {
    let root = directory();
    let parts = install(root.path(), true);
    let (device, key) = transport();
    let weak = VerifyingKey::from_bytes(&[0; 32]).unwrap();
    assert!(PinnedFolderAuthority::new(parts.writer_id(), weak).is_err());
    assert!(PinnedFolderAuthority::new(WriterId::from_uuid(Uuid::nil()), key).is_err());
    let wrong_local = PinnedFolderAuthority::new(
        parts.writer_id(),
        SigningKey::from_bytes(&[100; 32]).verifying_key(),
    )
    .unwrap();
    let reused_remote = PinnedFolderAuthority::new(
        WriterId::from_uuid(Uuid::from_u128(950)),
        parts.signing_key().verifying_key(),
    )
    .unwrap();
    for pin in [wrong_local, reused_remote] {
        assert!(FolderAuthorityConfig::create(&parts, pin, device, key, &protector()).is_err());
    }
    for bad_key in [weak, parts.signing_key().verifying_key()] {
        assert!(
            FolderAuthorityConfig::create(&parts, local_pin(&parts), device, bad_key, &protector())
                .is_err()
        );
    }
    assert!(
        FolderAuthorityConfig::create(
            &parts,
            local_pin(&parts),
            DeviceId::from_uuid(Uuid::nil()),
            key,
            &protector()
        )
        .is_err()
    );
    assert!(!root.path().join(RECORD_KEY).exists());
}

#[test]
fn private_file_and_root_lock_checks_prevent_substitution() {
    let root = directory();
    let parts = install(root.path(), true);
    create(&parts);
    let original = std::fs::read(root.path().join(RECORD_KEY)).unwrap();
    let displaced = root.path().join("displaced.v1");
    std::fs::rename(root.path().join(RECORD_KEY), &displaced).unwrap();
    symlink(&displaced, root.path().join(RECORD_KEY)).unwrap();
    assert!(FolderAuthorityConfig::open(&parts, &protector()).is_err());
    std::fs::remove_file(root.path().join(RECORD_KEY)).unwrap();
    std::fs::rename(&displaced, root.path().join(RECORD_KEY)).unwrap();
    std::fs::set_permissions(
        root.path().join(RECORD_KEY),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(FolderAuthorityConfig::open(&parts, &protector()).is_err());
    std::fs::set_permissions(
        root.path().join(RECORD_KEY),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    std::fs::rename(
        root.path().join("writer.lock"),
        root.path().join("old-lock.v1"),
    )
    .unwrap();
    assert!(FolderAuthorityConfig::open(&parts, &protector()).is_err());
    assert_eq!(
        std::fs::read(root.path().join(RECORD_KEY)).unwrap(),
        original
    );
}

#[test]
fn diagnostics_exclude_pinned_keys_and_identities() {
    let root = directory();
    let parts = install(root.path(), true);
    let config = create(&parts);
    let debug = format!(
        "{config:?} {:?} {}",
        config.authority(),
        AuthorityConfigError::Authentication
    );
    for canary in [
        parts.writer_id().to_string(),
        format!("{:?}", parts.signing_key().verifying_key().to_bytes()),
        transport().0.to_string(),
    ] {
        assert!(!debug.contains(&canary));
    }
}

#[test]
fn setup_binding_distinguishes_authenticated_identities_and_generation() {
    let root = directory();
    let mut parts = install(root.path(), true);
    let (device, key) = transport();
    let original = FolderAuthorityConfig::validated(&parts, local_pin(&parts), device, key)
        .unwrap()
        .setup_binding();
    let remote = PinnedFolderAuthority::new(
        WriterId::from_uuid(Uuid::from_u128(980)),
        SigningKey::from_bytes(&[110; 32]).verifying_key(),
    )
    .unwrap();
    for (authority, transport_device, transport_key) in [
        (remote, device, key),
        (
            local_pin(&parts),
            DeviceId::from_uuid(Uuid::from_u128(981)),
            key,
        ),
        (
            local_pin(&parts),
            device,
            SigningKey::from_bytes(&[111; 32]).verifying_key(),
        ),
    ] {
        assert_ne!(
            FolderAuthorityConfig::validated(&parts, authority, transport_device, transport_key)
                .unwrap()
                .setup_binding(),
            original
        );
    }
    parts.generation_id = Uuid::from_u128(982);
    assert_ne!(
        FolderAuthorityConfig::validated(&parts, local_pin(&parts), device, key)
            .unwrap()
            .setup_binding(),
        original
    );
    let remote_binding = |parts: &SyncInstallationParts| {
        FolderAuthorityConfig::validated(parts, remote, device, key)
            .unwrap()
            .setup_binding()
    };
    let baseline = remote_binding(&parts);
    let folder = parts.folder_id;
    parts.folder_id = FolderId::from_uuid(Uuid::from_u128(983));
    assert_ne!(remote_binding(&parts), baseline);
    parts.folder_id = folder;
    let installation = parts.installation_id;
    parts.installation_id = Uuid::from_u128(984);
    assert_ne!(remote_binding(&parts), baseline);
    parts.installation_id = installation;
    let writer = parts.writer_id;
    parts.writer_id = WriterId::from_uuid(Uuid::from_u128(985));
    assert_ne!(remote_binding(&parts), baseline);
    parts.writer_id = writer;
    let signing_key = std::mem::replace(&mut parts.signing_key, SigningKey::from_bytes(&[112; 32]));
    assert_ne!(remote_binding(&parts), baseline);
    parts.signing_key = signing_key;
    assert_eq!(remote_binding(&parts), baseline);
    assert!(!format!("{original:?}").contains(&format!("{:?}", original.as_bytes())));
}
