//! macOS bundle discovery for the pinned maintained sync engine.

use std::ffi::OsString;
use std::fs::{self, DirBuilder, File};
use std::io::Read as _;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::{FolderSyncRuntimeConfig, VerifiedEngineExecutable};

const MAX_MANIFEST_BYTES: u64 = 16 * 1024;
// macOS uses numeric loopback TLS; leave space for generated filenames
// beneath Darwin's filesystem path bound without imposing a Unix socket limit.
const MAX_RUNTIME_PARENT_BYTES: usize = 900;
const RUNTIME_DIRECTORY_NAME: &str = "cvs";
const MANIFEST_RELATIVE_PATH: &str = "../Resources/CovalentSyncEngine/manifest.json";
const GUARDIAN_NAME: &str = "covalent-engine-guardian";
const WORKER_NAME: &str = "covalent-rclone";
const ENGINE_LISTENER_PORT: u16 = 8789;

const ENGINE_VERSION: &str = "v1.75.1";
const ENGINE_COMMIT: &str = "687d264b689b8c49a67e2e52a8a5e0caa01c04ce";
const SOURCE_STATE: &str = "upstream-unmodified";
const UNSIGNED_ENGINE_SHA256: &str =
    "d606a368fe4b83b81080aa9913d9f24b382c601e995d25f63ab80a49e2c8dee9";
const GUARDIAN_SOURCE_SHA256: &str =
    "c50b5cf10a287c4c061b7891978fd2681b5c96910eb2c69c922beae349140579";

/// Fixed host errors never include a bundle path, manifest body, or digest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MacHostError {
    /// The app bundle contains incomplete, unsafe, or incorrectly pinned files.
    InvalidPackage,
    /// The private runtime parent is unavailable, unsafe, or too long.
    InvalidRuntimeDirectory,
}

/// Discover the package beside the running node helper.
///
/// `Ok(None)` means that none of the three packaged artifacts exists. Any
/// partial, unsafe, malformed, or incorrectly pinned package is an error and
/// must be surfaced as needs-attention rather than retried from another path.
pub fn discover_packaged_engine() -> Result<Option<FolderSyncRuntimeConfig>, MacHostError> {
    let executable = std::env::current_exe().map_err(|_| MacHostError::InvalidPackage)?;
    let runtime_parent = std::env::var_os("COVALENT_SYNC_RUNTIME_DIR");
    discover_at(&executable, runtime_parent)
}

fn discover_at(
    executable: &Path,
    runtime_parent_override: Option<OsString>,
) -> Result<Option<FolderSyncRuntimeConfig>, MacHostError> {
    let macos_directory = executable.parent().ok_or(MacHostError::InvalidPackage)?;
    let manifest = macos_directory.join(MANIFEST_RELATIVE_PATH);
    let guardian = macos_directory.join(GUARDIAN_NAME);
    let worker = macos_directory.join(WORKER_NAME);

    let artifacts = [manifest.as_path(), guardian.as_path(), worker.as_path()];
    let mut present = 0_usize;
    for artifact in artifacts {
        match fs::symlink_metadata(artifact) {
            Ok(_) => present += 1,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(MacHostError::InvalidPackage),
        }
    }
    if present == 0 {
        return Ok(None);
    }
    if present != artifacts.len() {
        return Err(MacHostError::InvalidPackage);
    }

    // The manifest's two resource ancestors are app content, never an alias
    // to an independently mutable directory outside the bundle.
    let contents = macos_directory
        .parent()
        .ok_or(MacHostError::InvalidPackage)?;
    for path in [
        contents.join("Resources"),
        contents.join("Resources/CovalentSyncEngine"),
    ] {
        if !fs::symlink_metadata(path)
            .map_err(|_| MacHostError::InvalidPackage)?
            .is_dir()
        {
            return Err(MacHostError::InvalidPackage);
        }
    }

    let parsed = read_manifest(&manifest)?;
    validate_manifest(&parsed)?;
    let guardian_digest = VerifiedEngineExecutable::parse_sha256_hex(
        &parsed.executables.covalent_engine_guardian.signed_sha256,
    )
    .map_err(|_| MacHostError::InvalidPackage)?;
    let worker_digest = VerifiedEngineExecutable::parse_sha256_hex(
        &parsed.executables.covalent_rclone.signed_sha256,
    )
    .map_err(|_| MacHostError::InvalidPackage)?;
    let guardian = VerifiedEngineExecutable::open(guardian, guardian_digest)
        .map_err(|_| MacHostError::InvalidPackage)?;
    let worker = VerifiedEngineExecutable::open(worker, worker_digest)
        .map_err(|_| MacHostError::InvalidPackage)?;

    let requested_runtime_parent = runtime_parent_override
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join(RUNTIME_DIRECTORY_NAME));
    let runtime_parent = prepare_runtime_parent(&requested_runtime_parent)?;
    Ok(Some(FolderSyncRuntimeConfig {
        guardian,
        worker,
        runtime_parent,
        listener: SocketAddr::V4(SocketAddrV4::new(
            Ipv4Addr::UNSPECIFIED,
            ENGINE_LISTENER_PORT,
        )),
        advertised_address: None,
    }))
}

fn prepare_runtime_parent(path: &Path) -> Result<PathBuf, MacHostError> {
    if !path.is_absolute() || path.as_os_str().as_bytes().len() > MAX_RUNTIME_PARENT_BYTES {
        return Err(MacHostError::InvalidRuntimeDirectory);
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_runtime_parent_metadata(&metadata)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            DirBuilder::new()
                .mode(0o700)
                .create(path)
                .map_err(|_| MacHostError::InvalidRuntimeDirectory)?;
        }
        Err(_) => return Err(MacHostError::InvalidRuntimeDirectory),
    }
    let canonical = fs::canonicalize(path).map_err(|_| MacHostError::InvalidRuntimeDirectory)?;
    if canonical.as_os_str().as_bytes().len() > MAX_RUNTIME_PARENT_BYTES {
        return Err(MacHostError::InvalidRuntimeDirectory);
    }
    let metadata =
        fs::symlink_metadata(&canonical).map_err(|_| MacHostError::InvalidRuntimeDirectory)?;
    validate_runtime_parent_metadata(&metadata)?;
    if !same_file(path, &canonical)? {
        return Err(MacHostError::InvalidRuntimeDirectory);
    }
    File::open(&canonical)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| MacHostError::InvalidRuntimeDirectory)?;
    Ok(canonical)
}

fn validate_runtime_parent_metadata(metadata: &fs::Metadata) -> Result<(), MacHostError> {
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o777 != 0o700
    {
        return Err(MacHostError::InvalidRuntimeDirectory);
    }
    Ok(())
}

fn same_file(first: &Path, second: &Path) -> Result<bool, MacHostError> {
    let first = fs::symlink_metadata(first).map_err(|_| MacHostError::InvalidRuntimeDirectory)?;
    let second = fs::symlink_metadata(second).map_err(|_| MacHostError::InvalidRuntimeDirectory)?;
    Ok((first.dev(), first.ino()) == (second.dev(), second.ino()))
}

fn read_manifest(path: &Path) -> Result<PackageManifest, MacHostError> {
    let descriptor = rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK,
        rustix::fs::Mode::empty(),
    )
    .map_err(|_| MacHostError::InvalidPackage)?;
    let mut file = File::from(descriptor);
    let before = file.metadata().map_err(|_| MacHostError::InvalidPackage)?;
    if !before.is_file()
        || before.uid() != rustix::process::geteuid().as_raw() && before.uid() != 0
        || before.mode() & 0o022 != 0
        || before.len() == 0
        || before.len() > MAX_MANIFEST_BYTES
    {
        return Err(MacHostError::InvalidPackage);
    }
    let length = usize::try_from(before.len()).map_err(|_| MacHostError::InvalidPackage)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|_| MacHostError::InvalidPackage)?;
    (&mut file)
        .take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| MacHostError::InvalidPackage)?;
    if bytes.len() != length {
        return Err(MacHostError::InvalidPackage);
    }
    let after = file.metadata().map_err(|_| MacHostError::InvalidPackage)?;
    if (
        before.dev(),
        before.ino(),
        before.len(),
        before.mode(),
        before.uid(),
        before.mtime(),
        before.mtime_nsec(),
        before.ctime(),
        before.ctime_nsec(),
    ) != (
        after.dev(),
        after.ino(),
        after.len(),
        after.mode(),
        after.uid(),
        after.mtime(),
        after.mtime_nsec(),
        after.ctime(),
        after.ctime_nsec(),
    ) {
        return Err(MacHostError::InvalidPackage);
    }
    serde_json::from_slice(&bytes).map_err(|_| MacHostError::InvalidPackage)
}

fn validate_manifest(manifest: &PackageManifest) -> Result<(), MacHostError> {
    if manifest.schema != 1
        || manifest.engine.name != "rclone"
        || manifest.engine.version != ENGINE_VERSION
        || manifest.engine.commit != ENGINE_COMMIT
        || manifest.engine.source_state != SOURCE_STATE
        || manifest.engine.go_version != "go1.26.7"
        || manifest.engine.unsigned_executable_sha256 != UNSIGNED_ENGINE_SHA256
        || manifest.guardian.source_sha256 != GUARDIAN_SOURCE_SHA256
        || manifest.executables.covalent_engine_guardian.architecture != "arm64"
        || manifest.executables.covalent_rclone.architecture != "arm64"
        || manifest.notices.provenance
            != "ce9886f37cd7bc7b62e755375f0d5f23d5f97f60670f6f857dac632b33f5e0ef"
        || manifest.notices.license
            != "9266eae9c6a441de0f6847f19ac8f09b280d55612b079eca03b3d49b822c1a71"
        || manifest.notices.source_build
            != "122b3ee828fb92c7149346ab02f306e53f6633cb907c5d05d7deac44ac307a2d"
        || manifest.notices.index
            != "59ab37c92bac32b1b3533ce8be192c282c9b7d25a81f03b2f63b7651faa4ab51"
        || manifest.notices.target_manifest
            != "9a624a53020218aea6927c2baa254daa7ce9099c83eeddcfed371001e889bd6d"
        || manifest.notices.combined
            != "9374aa12dc9da1416da235f6a664b6b5817081adf00b94a13d2c58d4a8ca68d4"
    {
        return Err(MacHostError::InvalidPackage);
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageManifest {
    schema: u8,
    engine: EngineRecord,
    guardian: GuardianRecord,
    notices: NoticeRecord,
    executables: ExecutableInventory,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EngineRecord {
    name: String,
    version: String,
    commit: String,
    source_state: String,
    go_version: String,
    unsigned_executable_sha256: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GuardianRecord {
    source_sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoticeRecord {
    #[serde(rename = "PROVENANCE.txt")]
    provenance: String,
    #[serde(rename = "rclone-LICENSE.txt")]
    license: String,
    #[serde(rename = "source-build.json")]
    source_build: String,
    #[serde(rename = "notices-index.txt")]
    index: String,
    #[serde(rename = "notices/manifest.json")]
    target_manifest: String,
    #[serde(rename = "notices/THIRD-PARTY-NOTICES.txt")]
    combined: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutableInventory {
    #[serde(rename = "covalent-engine-guardian")]
    covalent_engine_guardian: ExecutableRecord,
    #[serde(rename = "covalent-rclone")]
    covalent_rclone: ExecutableRecord,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ExecutableRecord {
    architecture: String,
    signed_sha256: String,
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;

    use sha2::{Digest as _, Sha256};
    use tempfile::TempDir;

    use super::*;

    fn digest(path: &Path) -> String {
        format!("{:x}", Sha256::digest(fs::read(path).unwrap()))
    }

    fn assert_error(
        result: Result<Option<FolderSyncRuntimeConfig>, MacHostError>,
        expected: MacHostError,
    ) {
        match result {
            Err(error) => assert_eq!(error, expected),
            Ok(_) => panic!("expected mac host error"),
        }
    }

    fn fixture() -> (TempDir, PathBuf, PathBuf) {
        let root = tempfile::Builder::new()
            .prefix("cv-host-")
            .tempdir_in("/tmp")
            .unwrap();
        let macos = root.path().join("Covalent.app/Contents/MacOS");
        let resources = root
            .path()
            .join("Covalent.app/Contents/Resources/CovalentSyncEngine");
        fs::create_dir_all(&macos).unwrap();
        fs::create_dir_all(&resources).unwrap();
        let executable = macos.join("covalent-node");
        for name in ["covalent-node", GUARDIAN_NAME, WORKER_NAME] {
            fs::write(macos.join(name), b"#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(macos.join(name), fs::Permissions::from_mode(0o755)).unwrap();
        }
        let signed = digest(&macos.join(GUARDIAN_NAME));
        let manifest = serde_json::json!({
            "schema": 1,
            "engine": {
                "name": "rclone",
                "version": ENGINE_VERSION,
                "commit": ENGINE_COMMIT,
                "sourceState": SOURCE_STATE,
                "goVersion": "go1.26.7",
                "unsignedExecutableSha256": UNSIGNED_ENGINE_SHA256,
            },
            "guardian": { "sourceSha256": GUARDIAN_SOURCE_SHA256 },
            "notices": {
                "PROVENANCE.txt": "ce9886f37cd7bc7b62e755375f0d5f23d5f97f60670f6f857dac632b33f5e0ef",
                "rclone-LICENSE.txt": "9266eae9c6a441de0f6847f19ac8f09b280d55612b079eca03b3d49b822c1a71",
                "source-build.json": "122b3ee828fb92c7149346ab02f306e53f6633cb907c5d05d7deac44ac307a2d",
                "notices-index.txt": "59ab37c92bac32b1b3533ce8be192c282c9b7d25a81f03b2f63b7651faa4ab51",
                "notices/manifest.json": "9a624a53020218aea6927c2baa254daa7ce9099c83eeddcfed371001e889bd6d",
                "notices/THIRD-PARTY-NOTICES.txt": "9374aa12dc9da1416da235f6a664b6b5817081adf00b94a13d2c58d4a8ca68d4",
            },
            "executables": {
                GUARDIAN_NAME: { "architecture": "arm64", "signedSha256": signed },
                WORKER_NAME: { "architecture": "arm64", "signedSha256": digest(&macos.join(WORKER_NAME)) },
            },
        });
        fs::write(
            resources.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let runtime = root.path().join("runtime");
        (root, executable, runtime)
    }

    #[test]
    fn valid_manifest_opens_exact_packaged_executables() {
        let (_root, executable, runtime) = fixture();
        let config = discover_at(&executable, Some(runtime.clone().into_os_string()))
            .unwrap()
            .unwrap();
        assert_eq!(config.runtime_parent, fs::canonicalize(runtime).unwrap());
        assert_eq!(config.listener, "0.0.0.0:8789".parse().unwrap());
        assert_eq!(config.advertised_address, None);
    }

    #[test]
    fn absence_is_distinct_from_every_partial_or_invalid_package() {
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("target/debug/covalent-node");
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        fs::write(&executable, b"unused").unwrap();
        assert!(discover_at(&executable, None).unwrap().is_none());

        fs::write(executable.parent().unwrap().join(WORKER_NAME), b"partial").unwrap();
        assert_error(discover_at(&executable, None), MacHostError::InvalidPackage);

        let (_root, executable, runtime) = fixture();
        let manifest = executable.parent().unwrap().join(MANIFEST_RELATIVE_PATH);
        fs::write(manifest, b"{}").unwrap();
        assert_error(
            discover_at(&executable, Some(runtime.into_os_string())),
            MacHostError::InvalidPackage,
        );
    }

    #[test]
    fn wrong_digest_and_symlink_never_fall_back_to_another_executable() {
        let (_root, executable, runtime) = fixture();
        fs::write(executable.parent().unwrap().join(WORKER_NAME), b"changed").unwrap();
        assert_error(
            discover_at(&executable, Some(runtime.clone().into_os_string())),
            MacHostError::InvalidPackage,
        );

        let (_root, executable, runtime) = fixture();
        let worker = executable.parent().unwrap().join(WORKER_NAME);
        fs::remove_file(&worker).unwrap();
        std::os::unix::fs::symlink("/usr/bin/true", worker).unwrap();
        assert_error(
            discover_at(&executable, Some(runtime.into_os_string())),
            MacHostError::InvalidPackage,
        );
    }

    #[test]
    fn every_source_build_and_notice_binding_is_required() {
        for path in [
            "/engine/sourceState",
            "/engine/goVersion",
            "/engine/unsignedExecutableSha256",
            "/notices/rclone-LICENSE.txt",
            "/notices/source-build.json",
            "/notices/notices-index.txt",
            "/notices/notices~1manifest.json",
            "/notices/notices~1THIRD-PARTY-NOTICES.txt",
        ] {
            let (_root, executable, runtime) = fixture();
            let manifest = executable.parent().unwrap().join(MANIFEST_RELATIVE_PATH);
            let mut value: serde_json::Value =
                serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
            *value.pointer_mut(path).expect("fixture field") =
                serde_json::Value::String("0".repeat(64));
            fs::write(&manifest, serde_json::to_vec(&value).unwrap()).unwrap();
            assert_error(
                discover_at(&executable, Some(runtime.into_os_string())),
                MacHostError::InvalidPackage,
            );
        }
    }

    #[test]
    fn runtime_parent_is_absolute_private_and_bounded() {
        let (_root, executable, runtime) = fixture();
        fs::create_dir(&runtime).unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o755)).unwrap();
        assert_error(
            discover_at(&executable, Some(runtime.into_os_string())),
            MacHostError::InvalidRuntimeDirectory,
        );

        let (_root, executable, _runtime) = fixture();
        let long = PathBuf::from(format!("/tmp/{}", "x".repeat(MAX_RUNTIME_PARENT_BYTES - 4)));
        assert_error(
            discover_at(&executable, Some(long.into_os_string())),
            MacHostError::InvalidRuntimeDirectory,
        );
    }

    #[test]
    fn runtime_parent_accepts_app_sandbox_paths_longer_than_unix_socket_limit() {
        let (root, executable, _) = fixture();
        let long = root.path().join("sandbox-temporary-directory-".repeat(5));
        assert!(long.as_os_str().as_bytes().len() > 100);
        let config = discover_at(&executable, Some(long.clone().into_os_string()))
            .expect("valid TLS runtime parent")
            .expect("packaged engine");
        assert_eq!(config.runtime_parent, fs::canonicalize(long).unwrap());
    }

    #[test]
    fn symlinked_resource_directory_is_rejected() {
        let (_root, executable, runtime) = fixture();
        let resources = executable
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("Resources");
        let renamed = resources.with_file_name("outside-resources");
        fs::rename(&resources, &renamed).unwrap();
        std::os::unix::fs::symlink(&renamed, &resources).unwrap();
        assert_error(
            discover_at(&executable, Some(runtime.into_os_string())),
            MacHostError::InvalidPackage,
        );
    }
}
