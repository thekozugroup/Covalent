use super::*;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

fn digest(path: &Path) -> [u8; 32] {
    let bytes = fs::read(path).expect("read fixture");
    Sha256::digest(bytes).into()
}

fn private_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("set fixture mode");
}

fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
    let root = tempfile::tempdir().expect("fixture directory");
    private_mode(root.path(), 0o700);
    let data = root.path().join("data");
    fs::create_dir(&data).expect("data directory");
    private_mode(&data, 0o700);
    let config = root.path().join("config.xml");
    fs::write(&config, b"<configuration/>\n").expect("config");
    private_mode(&config, 0o600);
    for name in ["cert.pem", "key.pem"] {
        let path = root.path().join(name);
        fs::write(path, b"fixture\n").expect("identity fixture");
        private_mode(&root.path().join(name), 0o600);
    }
    let guardian = root.path().join("guardian");
    fs::write(&guardian, b"#!/bin/sh\ncat >/dev/null\n").expect("guardian");
    private_mode(&guardian, 0o700);
    let engine = root.path().join("engine");
    fs::write(&engine, b"#!/bin/sh\nexit 0\n").expect("engine");
    private_mode(&engine, 0o700);
    (root, guardian, engine, config)
}

#[test]
fn executable_owner_policy_rejects_foreign_apps_and_scopes_android_system_uid() {
    let app_uid = 10_233;
    assert!(is_trusted_executable_owner(0, app_uid));
    assert!(is_trusted_executable_owner(app_uid, app_uid));
    for foreign in [1, 999, 1001, 10_234, 65_534] {
        assert!(!is_trusted_executable_owner(foreign, app_uid));
    }
    #[cfg(target_os = "android")]
    assert!(is_trusted_executable_owner(1000, app_uid));
    #[cfg(not(target_os = "android"))]
    assert!(!is_trusted_executable_owner(1000, app_uid));
}

#[test]
fn executable_checks_pin_digest_and_rejects_symlink_or_broad_mode() {
    let (root, guardian, _engine, _config) = fixture();
    let expected = digest(&guardian);
    let verified = VerifiedEngineExecutable::open(&guardian, expected).expect("verified fixture");
    assert_eq!(verified.path(), guardian);
    assert_eq!(
        VerifiedEngineExecutable::parse_sha256_hex(
            &expected
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        )
        .expect("hex digest"),
        expected
    );
    assert_eq!(
        VerifiedEngineExecutable::open(&guardian, [0; 32]).unwrap_err(),
        EngineSupervisorError::ExecutableDigestMismatch
    );

    private_mode(&guardian, 0o702);
    assert_eq!(
        VerifiedEngineExecutable::open(&guardian, expected).unwrap_err(),
        EngineSupervisorError::InvalidExecutable
    );
    private_mode(&guardian, 0o700);
    let link = root.path().join("guardian-link");
    std::os::unix::fs::symlink(&guardian, &link).expect("symlink fixture");
    assert_eq!(
        VerifiedEngineExecutable::open(&link, expected).unwrap_err(),
        EngineSupervisorError::InvalidExecutable
    );
}

#[tokio::test]
async fn worker_drop_closes_lifeline_and_reaper_reports_exit() {
    let (_root, guardian, engine, config) = fixture();
    let config_dir = config.parent().unwrap().to_owned();
    let data_dir = guardian.parent().unwrap().join("data");
    let guardian = VerifiedEngineExecutable::open(&guardian, digest(&guardian)).expect("guardian");
    let engine = VerifiedEngineExecutable::open(&engine, digest(&engine)).expect("engine");
    let mut worker =
        OwnedEngineWorker::launch(&guardian, &engine, config_dir, data_dir, Box::new(()))
            .expect("launch fixture");
    assert!(!worker.stop_started());
    let outcome = worker.stop().await.expect("reap fixture");
    assert!(matches!(outcome, StopOutcome::Exited(_)));
    assert!(worker.stop_started());
    assert!(worker.try_status().expect("status fixture").is_some());
    assert!(matches!(
        worker.stop().await.expect("repeat stop fixture"),
        StopOutcome::Exited(_)
    ));
}

struct DropProbe(Arc<AtomicBool>);

impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn keepalive_is_dropped_after_reap_even_when_worker_handle_drops() {
    let (root, _guardian, engine, config) = fixture();
    let slow_guardian = root.path().join("slow-guardian");
    fs::write(&slow_guardian, b"#!/bin/sh\nsleep 1\ncat >/dev/null\n").expect("slow guardian");
    private_mode(&slow_guardian, 0o700);
    let guardian = VerifiedEngineExecutable::open(&slow_guardian, digest(&slow_guardian))
        .expect("slow guardian");
    let engine = VerifiedEngineExecutable::open(&engine, digest(&engine)).expect("engine");
    let probe = Arc::new(AtomicBool::new(false));
    let worker = OwnedEngineWorker::launch(
        &guardian,
        &engine,
        config.parent().unwrap().to_owned(),
        root.path().join("data"),
        Box::new(DropProbe(probe.clone())),
    )
    .expect("launch fixture");
    drop(worker);
    assert!(!probe.load(Ordering::SeqCst));
    for _ in 0..30 {
        if probe.load(Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("reaper did not release keepalive");
}
