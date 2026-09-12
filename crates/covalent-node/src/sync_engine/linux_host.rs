//! Linux container discovery for the pinned maintained sync engine.

use std::ffi::OsStr;
use std::fs::{self, DirBuilder, File};
use std::io::Read as _;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use super::{FolderSyncRuntimeConfig, VerifiedEngineExecutable};

const MANIFEST: &str = "/usr/local/share/covalent/sync-engine/manifest.json";
const GUARDIAN: &str = "/usr/local/libexec/covalent-sync-engine/covalent-engine-guardian";
const WORKER: &str = "/usr/local/libexec/covalent-sync-engine/covalent-syncthing";
const RUNTIME_PARENT: &str = "/tmp/cvs";
const MAX_MANIFEST_BYTES: u64 = 8 * 1024;
const MAX_NOTICE_BYTES: u64 = 64 * 1024;
const MAX_COMBINED_NOTICE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TARGET_EVIDENCE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_BUILD_METADATA_BYTES: u64 = 1024 * 1024;
const MAX_RUNTIME_PARENT_BYTES: usize = 74;
const MIN_WORKER_BYTES: u64 = 16 * 1024 * 1024;
const MAX_WORKER_BYTES: u64 = 48 * 1024 * 1024;
const MIN_GUARDIAN_BYTES: u64 = 8 * 1024;
const MAX_GUARDIAN_BYTES: u64 = 128 * 1024;
const ENGINE_LISTENER_PORT: u16 = 8789;
const ENGINE_VERSION: &str = "v2.1.3";
const ENGINE_COMMIT: &str = "946e2b83a1f6c6ae119427c09e0a5802940b82ff";
const SOURCE_ARCHIVE_SHA256: &str =
    "dbcc9498602286a843f29a7104833bd1422082999aa51ff92eef493172d47959";
const SOURCE_PATCH_SHA256: &str =
    "e58e7d133a388576a54cacc6a5a5094e6607c483c0daabac552de1a1854d92ac";
const SOURCE_STATE: &str = "modified";
const GUARDIAN_SOURCE_SHA256: &str =
    "c50b5cf10a287c4c061b7891978fd2681b5c96910eb2c69c922beae349140579";
const LICENSE_SHA256: &str = "3f3d9e0024b1921b067d6f7f88deb4a60cbe7a78e76c64e3f1d7fc3b779b9d04";
const AUTHORS_SHA256: &str = "5a0044d13ddf6f013bdd5c2bc419bf45d6123c356567510237e82f304d113d48";

/// Fixed host errors never include image paths, manifest bytes, or hashes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinuxHostError {
    /// The image contains incomplete, unsafe, or incorrectly pinned files.
    InvalidPackage,
    /// The short private socket parent is unavailable or unsafe.
    InvalidRuntimeDirectory,
    /// An operator-supplied sync address is not a concrete numeric endpoint.
    InvalidAdvertisedAddress,
}

/// Discover the immutable package installed in the container image.
///
/// `Ok(None)` is reserved for a developer binary with no package files. Any
/// partial or damaged image is an error that the caller maps to folder-sync
/// needs-attention while the backup runtime continues.
pub fn discover_packaged_engine() -> Result<Option<FolderSyncRuntimeConfig>, LinuxHostError> {
    let mut package = discover_at(
        Path::new(MANIFEST),
        Path::new(GUARDIAN),
        Path::new(WORKER),
        Path::new(RUNTIME_PARENT),
    )?;
    if let Some(configuration) = &mut package {
        configuration.advertised_address = parse_advertised_address(
            std::env::var_os("COVALENT_SYNC_ADVERTISED_ADDRESS").as_deref(),
        )?;
    }
    Ok(package)
}

fn parse_advertised_address(value: Option<&OsStr>) -> Result<Option<SocketAddr>, LinuxHostError> {
    let Some(value) = value else { return Ok(None) };
    let value = value
        .to_str()
        .ok_or(LinuxHostError::InvalidAdvertisedAddress)?;
    if value.is_empty() {
        return Ok(None);
    }
    if value.len() > 80 {
        return Err(LinuxHostError::InvalidAdvertisedAddress);
    }
    let address: SocketAddr = value
        .parse()
        .map_err(|_| LinuxHostError::InvalidAdvertisedAddress)?;
    super::config::validate_peer_address(address)
        .map_err(|_| LinuxHostError::InvalidAdvertisedAddress)?;
    if let SocketAddr::V6(address) = address
        && (address.scope_id() != 0 || address.flowinfo() != 0)
    {
        return Err(LinuxHostError::InvalidAdvertisedAddress);
    }
    Ok(Some(address))
}

fn discover_at(
    manifest_path: &Path,
    guardian_path: &Path,
    worker_path: &Path,
    runtime_parent: &Path,
) -> Result<Option<FolderSyncRuntimeConfig>, LinuxHostError> {
    let notices = [
        manifest_path.with_file_name("Syncthing-LICENSE.txt"),
        manifest_path.with_file_name("Syncthing-AUTHORS.txt"),
        manifest_path.with_file_name("PROVENANCE.txt"),
    ];
    let artifacts = [
        manifest_path,
        guardian_path,
        worker_path,
        notices[0].as_path(),
        notices[1].as_path(),
        notices[2].as_path(),
    ];
    let mut present = 0_usize;
    for artifact in artifacts {
        match fs::symlink_metadata(artifact) {
            Ok(_) => present += 1,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(LinuxHostError::InvalidPackage),
        }
    }
    if present == 0 {
        return Ok(None);
    }
    if present != artifacts.len() {
        return Err(LinuxHostError::InvalidPackage);
    }
    for parent in [manifest_path.parent(), guardian_path.parent()] {
        let parent = parent.ok_or(LinuxHostError::InvalidPackage)?;
        if !fs::symlink_metadata(parent)
            .map_err(|_| LinuxHostError::InvalidPackage)?
            .is_dir()
            || fs::canonicalize(parent).map_err(|_| LinuxHostError::InvalidPackage)? != parent
        {
            return Err(LinuxHostError::InvalidPackage);
        }
    }

    // Notices are a distribution obligation and package-integrity signal. The
    // executable authorization still comes only from the strict manifest.
    let license = read_bounded_file(&notices[0], MAX_NOTICE_BYTES)?;
    let authors = read_bounded_file(&notices[1], MAX_NOTICE_BYTES)?;
    let _provenance = read_bounded_file(&notices[2], MAX_NOTICE_BYTES)?;
    let license_digest: [u8; 32] = Sha256::digest(&license).into();
    let authors_digest: [u8; 32] = Sha256::digest(&authors).into();
    if license_digest != parse_lower_hex(LICENSE_SHA256)?
        || authors_digest != parse_lower_hex(AUTHORS_SHA256)?
    {
        return Err(LinuxHostError::InvalidPackage);
    }
    let manifest_bytes = read_bounded_file(manifest_path, MAX_MANIFEST_BYTES)?;
    let manifest: PackageManifest =
        serde_json::from_slice(&manifest_bytes).map_err(|_| LinuxHostError::InvalidPackage)?;
    validate_manifest(&manifest)?;
    validate_target_notices(manifest_path, &manifest)?;
    validate_packaged_size(guardian_path, manifest.executables.guardian.bytes)?;
    validate_packaged_size(worker_path, manifest.executables.worker.bytes)?;
    let guardian_digest = parse_lower_hex(&manifest.executables.guardian.sha256)?;
    let worker_digest = parse_lower_hex(&manifest.executables.worker.sha256)?;
    let guardian = VerifiedEngineExecutable::open(guardian_path, guardian_digest)
        .map_err(|_| LinuxHostError::InvalidPackage)?;
    let worker = VerifiedEngineExecutable::open(worker_path, worker_digest)
        .map_err(|_| LinuxHostError::InvalidPackage)?;
    let runtime_parent = prepare_runtime_parent(runtime_parent)?;
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

fn validate_manifest(manifest: &PackageManifest) -> Result<(), LinuxHostError> {
    if manifest.schema != 1
        || manifest.engine.name != "Syncthing"
        || manifest.engine.version != ENGINE_VERSION
        || manifest.engine.commit != ENGINE_COMMIT
        || manifest.engine.source_archive_sha256 != SOURCE_ARCHIVE_SHA256
        || manifest.engine.source_patch_sha256 != SOURCE_PATCH_SHA256
        || manifest.engine.source_state != SOURCE_STATE
        || manifest.engine.go_version != "go1.26.7"
        || manifest.guardian.source_sha256 != GUARDIAN_SOURCE_SHA256
        || manifest.notices.license_sha256 != LICENSE_SHA256
        || manifest.notices.authors_sha256 != AUTHORS_SHA256
        || manifest.notices.third_party_status != "target-texts-collected-review-required"
        || manifest.architecture != std::env::consts::ARCH
        || !(MIN_WORKER_BYTES..=MAX_WORKER_BYTES).contains(&manifest.executables.worker.bytes)
        || !(MIN_GUARDIAN_BYTES..=MAX_GUARDIAN_BYTES).contains(&manifest.executables.guardian.bytes)
    {
        return Err(LinuxHostError::InvalidPackage);
    }
    Ok(())
}

fn validate_target_notices(
    manifest_path: &Path,
    manifest: &PackageManifest,
) -> Result<(), LinuxHostError> {
    let share = manifest_path
        .parent()
        .ok_or(LinuxHostError::InvalidPackage)?;
    for name in ["notices", "evidence"] {
        let directory = share.join(name);
        if !fs::symlink_metadata(&directory)
            .map_err(|_| LinuxHostError::InvalidPackage)?
            .is_dir()
            || fs::canonicalize(&directory).map_err(|_| LinuxHostError::InvalidPackage)?
                != directory
        {
            return Err(LinuxHostError::InvalidPackage);
        }
    }
    // Verify the complete readable license text and the exact target graph
    // retained by the image build, before authorizing either executable.
    let evidence = &manifest.target_evidence;
    for (name, digest, expected, maximum) in [
        (
            "notices/manifest.json",
            &manifest.notices.target_manifest_sha256,
            None,
            MAX_TARGET_EVIDENCE_BYTES,
        ),
        (
            "THIRD-PARTY-NOTICES.txt",
            &manifest.notices.combined_sha256,
            Some(manifest.notices.combined_bytes),
            MAX_COMBINED_NOTICE_BYTES,
        ),
        (
            "evidence/go-target-deps.ndjson",
            &evidence.go_list_sha256,
            Some(evidence.go_list_bytes),
            MAX_TARGET_EVIDENCE_BYTES,
        ),
        (
            "evidence/go-version.txt",
            &evidence.go_version_sha256,
            Some(evidence.go_version_bytes),
            MAX_BUILD_METADATA_BYTES,
        ),
        (
            "evidence/target-license-inventory.json",
            &evidence.license_inventory_sha256,
            Some(evidence.license_inventory_bytes),
            MAX_TARGET_EVIDENCE_BYTES,
        ),
    ] {
        if expected.is_some_and(|bytes| bytes == 0 || bytes > maximum) {
            return Err(LinuxHostError::InvalidPackage);
        }
        let bytes = read_bounded_file(&share.join(name), maximum)?;
        let observed: [u8; 32] = Sha256::digest(&bytes).into();
        if expected.is_some_and(|expected| expected != bytes.len() as u64)
            || observed != parse_lower_hex(digest)?
        {
            return Err(LinuxHostError::InvalidPackage);
        }
    }
    Ok(())
}

fn parse_lower_hex(value: &str) -> Result<[u8; 32], LinuxHostError> {
    if value.len() != 64
        || !value
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    {
        return Err(LinuxHostError::InvalidPackage);
    }
    VerifiedEngineExecutable::parse_sha256_hex(value).map_err(|_| LinuxHostError::InvalidPackage)
}

fn validate_packaged_size(path: &Path, expected: u64) -> Result<(), LinuxHostError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| LinuxHostError::InvalidPackage)?;
    if !metadata.is_file() || metadata.len() != expected {
        return Err(LinuxHostError::InvalidPackage);
    }
    Ok(())
}

fn read_bounded_file(path: &Path, maximum: u64) -> Result<Vec<u8>, LinuxHostError> {
    let descriptor = rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK,
        rustix::fs::Mode::empty(),
    )
    .map_err(|_| LinuxHostError::InvalidPackage)?;
    let mut file = File::from(descriptor);
    let before = file
        .metadata()
        .map_err(|_| LinuxHostError::InvalidPackage)?;
    let current_uid = rustix::process::geteuid().as_raw();
    if !before.is_file()
        || before.uid() != 0 && before.uid() != current_uid
        || before.mode() & 0o022 != 0
        || before.len() == 0
        || before.len() > maximum
    {
        return Err(LinuxHostError::InvalidPackage);
    }
    let length = usize::try_from(before.len()).map_err(|_| LinuxHostError::InvalidPackage)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|_| LinuxHostError::InvalidPackage)?;
    (&mut file)
        .take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| LinuxHostError::InvalidPackage)?;
    if bytes.len() != length {
        return Err(LinuxHostError::InvalidPackage);
    }
    let after = file
        .metadata()
        .map_err(|_| LinuxHostError::InvalidPackage)?;
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
        return Err(LinuxHostError::InvalidPackage);
    }
    Ok(bytes)
}

fn prepare_runtime_parent(path: &Path) -> Result<PathBuf, LinuxHostError> {
    if !path.is_absolute() || path.as_os_str().as_bytes().len() > MAX_RUNTIME_PARENT_BYTES {
        return Err(LinuxHostError::InvalidRuntimeDirectory);
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_runtime_metadata(&metadata)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            DirBuilder::new()
                .mode(0o700)
                .create(path)
                .map_err(|_| LinuxHostError::InvalidRuntimeDirectory)?;
        }
        Err(_) => return Err(LinuxHostError::InvalidRuntimeDirectory),
    }
    let canonical = fs::canonicalize(path).map_err(|_| LinuxHostError::InvalidRuntimeDirectory)?;
    let metadata =
        fs::symlink_metadata(&canonical).map_err(|_| LinuxHostError::InvalidRuntimeDirectory)?;
    validate_runtime_metadata(&metadata)?;
    let lexical =
        fs::symlink_metadata(path).map_err(|_| LinuxHostError::InvalidRuntimeDirectory)?;
    if (lexical.dev(), lexical.ino()) != (metadata.dev(), metadata.ino())
        || canonical.as_os_str().as_bytes().len() > MAX_RUNTIME_PARENT_BYTES
    {
        return Err(LinuxHostError::InvalidRuntimeDirectory);
    }
    File::open(&canonical)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| LinuxHostError::InvalidRuntimeDirectory)?;
    Ok(canonical)
}

fn validate_runtime_metadata(metadata: &fs::Metadata) -> Result<(), LinuxHostError> {
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o777 != 0o700
    {
        return Err(LinuxHostError::InvalidRuntimeDirectory);
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PackageManifest {
    schema: u8,
    engine: EngineRecord,
    guardian: GuardianRecord,
    notices: NoticeRecord,
    target_evidence: TargetEvidence,
    architecture: String,
    executables: ExecutableInventory,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EngineRecord {
    name: String,
    version: String,
    commit: String,
    source_archive_sha256: String,
    source_patch_sha256: String,
    source_state: String,
    go_version: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GuardianRecord {
    source_sha256: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NoticeRecord {
    license_sha256: String,
    authors_sha256: String,
    third_party_status: String,
    target_manifest_sha256: String,
    combined_sha256: String,
    combined_bytes: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TargetEvidence {
    go_list_sha256: String,
    go_list_bytes: u64,
    go_version_sha256: String,
    go_version_bytes: u64,
    license_inventory_sha256: String,
    license_inventory_bytes: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutableInventory {
    guardian: ExecutableRecord,
    worker: ExecutableRecord,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutableRecord {
    sha256: String,
    bytes: u64,
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    use super::*;

    #[test]
    fn advertised_sync_address_preserves_mapped_port_and_rejects_unsafe_values() {
        assert_eq!(parse_advertised_address(None), Ok(None));
        assert_eq!(parse_advertised_address(Some(OsStr::new(""))), Ok(None));
        for value in ["192.168.1.50:18789", "[fd00::1234]:28789"] {
            assert_eq!(
                parse_advertised_address(Some(OsStr::new(value))),
                Ok(Some(value.parse().unwrap()))
            );
        }
        for value in [
            "atlas:8789",
            "tcp://192.168.1.50:8789",
            "0.0.0.0:8789",
            "[::]:8789",
            "224.0.0.1:8789",
            "192.168.1.50:0",
            "[fe80::1%2]:8789",
            " 192.168.1.50:8789",
            "192.168.1.50:8789\n",
        ] {
            assert_eq!(
                parse_advertised_address(Some(OsStr::new(value))),
                Err(LinuxHostError::InvalidAdvertisedAddress)
            );
        }
        assert_eq!(
            parse_advertised_address(Some(OsStr::from_bytes(&[0xff]))),
            Err(LinuxHostError::InvalidAdvertisedAddress)
        );
    }

    struct Fixture {
        _root: tempfile::TempDir,
        manifest: PathBuf,
        guardian: PathBuf,
        worker: PathBuf,
        runtime: PathBuf,
    }

    fn digest_file(path: &Path) -> String {
        let mut file = File::open(path).unwrap();
        let mut digest = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let count = file.read(&mut buffer).unwrap();
            if count == 0 {
                break;
            }
            digest.update(&buffer[..count]);
        }
        format!("{:x}", digest.finalize())
    }

    fn executable(path: &Path, bytes: u64) {
        let mut file = File::create(path).unwrap();
        file.write_all(b"fixture").unwrap();
        file.set_len(bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o555)).unwrap();
    }

    fn assert_error(
        result: Result<Option<FolderSyncRuntimeConfig>, LinuxHostError>,
        expected: LinuxHostError,
    ) {
        match result {
            Err(error) => assert_eq!(error, expected),
            Ok(_) => panic!("unsafe package unexpectedly produced a host result"),
        }
    }

    fn fixture() -> Fixture {
        let root = tempfile::Builder::new()
            .prefix("cv-linux-host-")
            .tempdir_in("/tmp")
            .unwrap();
        let canonical_root = fs::canonicalize(root.path()).unwrap();
        let bin = canonical_root.join("libexec");
        let share = canonical_root.join("share");
        fs::create_dir(&bin).unwrap();
        fs::create_dir(&share).unwrap();
        let guardian = bin.join("covalent-engine-guardian");
        let worker = bin.join("covalent-syncthing");
        executable(&guardian, MIN_GUARDIAN_BYTES);
        executable(&worker, MIN_WORKER_BYTES);
        fs::write(
            share.join("Syncthing-LICENSE.txt"),
            include_bytes!(
                "../../../../packaging/docker/sync-engine-notices/Syncthing-LICENSE.txt"
            ),
        )
        .unwrap();
        fs::write(
            share.join("Syncthing-AUTHORS.txt"),
            include_bytes!(
                "../../../../packaging/docker/sync-engine-notices/Syncthing-AUTHORS.txt"
            ),
        )
        .unwrap();
        fs::write(
            share.join("PROVENANCE.txt"),
            b"bounded fixture provenance\n",
        )
        .unwrap();
        fs::create_dir(share.join("notices")).unwrap();
        fs::create_dir(share.join("evidence")).unwrap();
        for name in [
            "notices/manifest.json",
            "THIRD-PARTY-NOTICES.txt",
            "evidence/go-target-deps.ndjson",
            "evidence/go-version.txt",
            "evidence/target-license-inventory.json",
        ] {
            fs::write(share.join(name), b"fixture evidence\n").unwrap();
        }
        let evidence_sha = digest_file(&share.join("notices/manifest.json"));
        let evidence_bytes = b"fixture evidence\n".len();
        let manifest = share.join("manifest.json");
        fs::write(
            &manifest,
            serde_json::to_vec(&serde_json::json!({
                "schema": 1,
                "engine": {
                    "name": "Syncthing",
                    "version": ENGINE_VERSION,
                    "commit": ENGINE_COMMIT,
                    "sourceArchiveSha256": SOURCE_ARCHIVE_SHA256,
                    "sourcePatchSha256": SOURCE_PATCH_SHA256,
                    "sourceState": SOURCE_STATE,
                    "goVersion": "go1.26.7",
                },
                "guardian": { "sourceSha256": GUARDIAN_SOURCE_SHA256 },
                "notices": {
                    "licenseSha256": LICENSE_SHA256,
                    "authorsSha256": AUTHORS_SHA256,
                    "thirdPartyStatus": "target-texts-collected-review-required",
                    "targetManifestSha256": evidence_sha,
                    "combinedSha256": evidence_sha,
                    "combinedBytes": evidence_bytes,
                },
                "targetEvidence": {
                    "goListSha256": evidence_sha,
                    "goListBytes": evidence_bytes,
                    "goVersionSha256": evidence_sha,
                    "goVersionBytes": evidence_bytes,
                    "licenseInventorySha256": evidence_sha,
                    "licenseInventoryBytes": evidence_bytes,
                },
                "architecture": std::env::consts::ARCH,
                "executables": {
                    "guardian": {
                        "sha256": digest_file(&guardian),
                        "bytes": MIN_GUARDIAN_BYTES,
                    },
                    "worker": {
                        "sha256": digest_file(&worker),
                        "bytes": MIN_WORKER_BYTES,
                    },
                },
            }))
            .unwrap(),
        )
        .unwrap();
        let runtime = canonical_root.join("runtime");
        Fixture {
            _root: root,
            manifest,
            guardian,
            worker,
            runtime,
        }
    }

    #[test]
    fn exact_image_package_opens_helpers_and_private_runtime() {
        let fixture = fixture();
        let config = discover_at(
            &fixture.manifest,
            &fixture.guardian,
            &fixture.worker,
            &fixture.runtime,
        )
        .unwrap()
        .unwrap();
        assert_eq!(config.listener, "0.0.0.0:8789".parse().unwrap());
        assert_eq!(config.advertised_address, None);
        assert_eq!(
            config.runtime_parent,
            fs::canonicalize(fixture.runtime).unwrap()
        );
    }

    #[test]
    fn source_provenance_must_be_complete_exact_and_closed() {
        for (field, value) in [
            ("sourcePatchSha256", serde_json::json!("0".repeat(64))),
            ("sourceState", serde_json::json!("unmodified")),
        ] {
            let fixture = fixture();
            let mut manifest: serde_json::Value =
                serde_json::from_slice(&fs::read(&fixture.manifest).unwrap()).unwrap();
            manifest["engine"][field] = value;
            fs::write(&fixture.manifest, serde_json::to_vec(&manifest).unwrap()).unwrap();
            assert_error(
                discover_at(
                    &fixture.manifest,
                    &fixture.guardian,
                    &fixture.worker,
                    &fixture.runtime,
                ),
                LinuxHostError::InvalidPackage,
            );
            assert!(!fixture.runtime.exists());
        }

        for field in ["sourcePatchSha256", "sourceState"] {
            let fixture = fixture();
            let mut manifest: serde_json::Value =
                serde_json::from_slice(&fs::read(&fixture.manifest).unwrap()).unwrap();
            manifest["engine"].as_object_mut().unwrap().remove(field);
            fs::write(&fixture.manifest, serde_json::to_vec(&manifest).unwrap()).unwrap();
            assert_error(
                discover_at(
                    &fixture.manifest,
                    &fixture.guardian,
                    &fixture.worker,
                    &fixture.runtime,
                ),
                LinuxHostError::InvalidPackage,
            );
            assert!(!fixture.runtime.exists());
        }

        let fixture = fixture();
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&fixture.manifest).unwrap()).unwrap();
        manifest["engine"]["unreviewedSource"] = serde_json::json!(true);
        fs::write(&fixture.manifest, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert_error(
            discover_at(
                &fixture.manifest,
                &fixture.guardian,
                &fixture.worker,
                &fixture.runtime,
            ),
            LinuxHostError::InvalidPackage,
        );
        assert!(!fixture.runtime.exists());
    }

    #[test]
    fn absent_is_distinct_from_partial_or_tampered_package() {
        let root = tempfile::tempdir().unwrap();
        let manifest = root.path().join("share/manifest.json");
        let guardian = root.path().join("bin/guardian");
        let worker = root.path().join("bin/worker");
        assert!(
            discover_at(&manifest, &guardian, &worker, &root.path().join("run"))
                .unwrap()
                .is_none()
        );
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        fs::write(&manifest, b"{}").unwrap();
        assert_error(
            discover_at(&manifest, &guardian, &worker, &root.path().join("run")),
            LinuxHostError::InvalidPackage,
        );

        let fixture = fixture();
        fs::set_permissions(&fixture.worker, fs::Permissions::from_mode(0o644)).unwrap();
        let mut worker = fs::OpenOptions::new()
            .write(true)
            .open(&fixture.worker)
            .unwrap();
        worker.write_all(b"F").unwrap();
        worker.sync_all().unwrap();
        fs::set_permissions(&fixture.worker, fs::Permissions::from_mode(0o555)).unwrap();
        assert_error(
            discover_at(
                &fixture.manifest,
                &fixture.guardian,
                &fixture.worker,
                &fixture.runtime,
            ),
            LinuxHostError::InvalidPackage,
        );
    }

    #[test]
    fn missing_or_tampered_target_notices_never_authorize_helpers() {
        for name in [
            "notices/manifest.json",
            "THIRD-PARTY-NOTICES.txt",
            "evidence/go-target-deps.ndjson",
            "evidence/go-version.txt",
            "evidence/target-license-inventory.json",
        ] {
            let fixture = fixture();
            let path = fixture.manifest.parent().unwrap().join(name);
            fs::write(&path, b"replaced evidence\n").unwrap();
            assert_error(
                discover_at(
                    &fixture.manifest,
                    &fixture.guardian,
                    &fixture.worker,
                    &fixture.runtime,
                ),
                LinuxHostError::InvalidPackage,
            );
            assert!(!fixture.runtime.exists());
            fs::remove_file(path).unwrap();
            assert_error(
                discover_at(
                    &fixture.manifest,
                    &fixture.guardian,
                    &fixture.worker,
                    &fixture.runtime,
                ),
                LinuxHostError::InvalidPackage,
            );
        }
    }

    #[test]
    fn symlinked_target_evidence_directory_is_rejected() {
        let fixture = fixture();
        let share = fixture.manifest.parent().unwrap();
        fs::rename(share.join("evidence"), share.join("moved-evidence")).unwrap();
        symlink(share.join("moved-evidence"), share.join("evidence")).unwrap();
        assert_error(
            discover_at(
                &fixture.manifest,
                &fixture.guardian,
                &fixture.worker,
                &fixture.runtime,
            ),
            LinuxHostError::InvalidPackage,
        );
        assert!(!fixture.runtime.exists());
    }

    #[test]
    fn unsafe_or_long_runtime_parent_is_never_repaired() {
        let fixture = fixture();
        fs::create_dir(&fixture.runtime).unwrap();
        fs::set_permissions(&fixture.runtime, fs::Permissions::from_mode(0o755)).unwrap();
        assert_error(
            discover_at(
                &fixture.manifest,
                &fixture.guardian,
                &fixture.worker,
                &fixture.runtime,
            ),
            LinuxHostError::InvalidRuntimeDirectory,
        );
        assert_eq!(
            fs::metadata(&fixture.runtime).unwrap().permissions().mode() & 0o777,
            0o755,
        );

        fs::remove_dir(&fixture.runtime).unwrap();
        let unrelated = fixture.runtime.with_file_name("unrelated");
        fs::create_dir(&unrelated).unwrap();
        fs::set_permissions(&unrelated, fs::Permissions::from_mode(0o700)).unwrap();
        symlink(&unrelated, &fixture.runtime).unwrap();
        assert_error(
            discover_at(
                &fixture.manifest,
                &fixture.guardian,
                &fixture.worker,
                &fixture.runtime,
            ),
            LinuxHostError::InvalidRuntimeDirectory,
        );
        assert_eq!(
            fs::metadata(&unrelated).unwrap().permissions().mode() & 0o777,
            0o700,
        );

        let too_long = PathBuf::from(format!("/tmp/{}", "x".repeat(MAX_RUNTIME_PARENT_BYTES)));
        assert_eq!(
            prepare_runtime_parent(&too_long).unwrap_err(),
            LinuxHostError::InvalidRuntimeDirectory,
        );
    }
}
