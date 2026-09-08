use super::*;

use std::fs::{self, File, OpenOptions};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime};

use tempfile::TempDir;

const VERSION_TIME: &str = "2026-09-08T03:57:54-04:00";
const MOD_TIME: &str = "2026-09-08T03:57:54-04:00";

#[test]
fn pinned_rfc3339_metadata_maps_to_the_upstream_simple_tag_and_epoch() {
    let parsed = parse_rfc3339(VERSION_TIME).expect("valid engine metadata");
    assert_eq!(parsed.local_tag(), "20260908-035754");
    assert_eq!(parsed.unix_seconds, 1_788_854_274);
    assert_eq!(
        tagged_filename("draft.tar.gz", &parsed.local_tag()).expect("tagged filename"),
        "draft.tar~20260908-035754.gz"
    );
    assert_eq!(
        tagged_filename(".profile", &parsed.local_tag()).expect("dotfile tag"),
        "~20260908-035754.profile"
    );
}

struct Fixture {
    temporary: TempDir,
    selected: SelectedVersion,
    archive: std::path::PathBuf,
    current: std::path::PathBuf,
}

impl Fixture {
    fn new(relative: &str, archived: &[u8], current_bytes: &[u8]) -> Self {
        let temporary = TempDir::new().expect("folder root");
        let root = temporary.path();
        let components = parse_relative_path(relative).expect("safe fixture path");
        let parent = &components[..components.len() - 1];
        let folder_parent = parent
            .iter()
            .fold(root.to_owned(), |path, component| path.join(component));
        let archive_parent = parent
            .iter()
            .fold(root.join(".stversions"), |path, component| {
                path.join(component)
            });
        fs::create_dir_all(&folder_parent).expect("current parent");
        fs::create_dir_all(&archive_parent).expect("archive parent");
        let tag = parse_rfc3339(VERSION_TIME).expect("time").local_tag();
        let archive = archive_parent
            .join(tagged_filename(components.last().unwrap(), &tag).expect("archive name"));
        let current = folder_parent.join(components.last().unwrap());
        fs::write(&archive, archived).expect("archive bytes");
        fs::write(&current, current_bytes).expect("current bytes");
        let modified = SystemTime::UNIX_EPOCH
            + Duration::from_secs(parse_rfc3339(MOD_TIME).expect("time").unix_seconds as u64);
        File::open(&archive)
            .expect("archive file")
            .set_times(fs::FileTimes::new().set_modified(modified))
            .expect("archive mtime");
        Self {
            temporary,
            selected: SelectedVersion::from_complete_listing(
                relative,
                &[VersionMetadata {
                    version_time: VERSION_TIME.to_owned(),
                    mod_time: MOD_TIME.to_owned(),
                    size: archived.len() as u64,
                }],
                0,
            )
            .expect("unambiguous fixture selection"),
            archive,
            current,
        }
    }

    fn request<'a>(
        &'a self,
        destination: &'a str,
        control: &'a JobControl,
    ) -> RecoverAsCopyRequest<'a> {
        RecoverAsCopyRequest {
            folder_root: self.temporary.path(),
            versioner: SimpleVersionerConfig::pinned_default(),
            selected: &self.selected,
            destination_basename: destination,
            maximum_bytes: MAX_RECOVERY_FILE_BYTES,
            control,
        }
    }

    fn destination(&self, name: &str) -> std::path::PathBuf {
        self.current.parent().expect("file parent").join(name)
    }
}

#[test]
fn recovers_selected_archive_as_fresh_copy_and_preserves_current_and_archive() {
    let archived = b"selected old archive bytes\n";
    let current = b"current remote replacement bytes\n";
    let fixture = Fixture::new("reports/ordinary.txt", archived, current);
    let control = JobControl::new();

    let receipt = recover_selected_version_as_copy(
        fixture.request("ordinary (Recovered Copy).txt", &control),
    )
    .expect("recover fresh copy");

    assert_eq!(
        receipt.destination,
        std::path::PathBuf::from("reports/ordinary (Recovered Copy).txt")
    );
    assert_eq!(receipt.size, archived.len() as u64);
    assert_eq!(receipt.sha256, <[u8; 32]>::from(Sha256::digest(archived)));
    assert_eq!(fs::read(&fixture.current).expect("current"), current);
    assert_eq!(fs::read(&fixture.archive).expect("archive"), archived);
    assert_eq!(
        fs::read(fixture.destination("ordinary (Recovered Copy).txt")).expect("copy"),
        archived
    );
}

#[test]
fn ordinary_current_edit_does_not_change_the_held_archive_copy() {
    let archived = b"selected archive bytes\n";
    let fixture = Fixture::new("concurrent.txt", archived, b"current before edit\n");
    let control = JobControl::new();
    let current = fixture.current.clone();

    let receipt = recover_with_hooks(
        fixture.request("concurrent (Recovered Copy).txt", &control),
        &mut || {
            fs::write(&current, b"ordinary local editor update\n").expect("current edit");
            Ok(())
        },
        &mut || Ok(()),
        &mut || {},
    )
    .expect("recover archive despite current edit");

    assert_eq!(receipt.sha256, <[u8; 32]>::from(Sha256::digest(archived)));
    assert_eq!(
        fs::read(&fixture.current).expect("edited current"),
        b"ordinary local editor update\n"
    );
    assert_eq!(fs::read(&fixture.archive).expect("archive"), archived);
    assert_eq!(
        fs::read(fixture.destination("concurrent (Recovered Copy).txt")).expect("copy"),
        archived
    );
}

#[test]
fn upstream_second_precision_matches_archive_with_retained_nanoseconds() {
    let fixture = Fixture::new("precision.txt", b"archive", b"current");
    let control = JobControl::new();
    let modified = SystemTime::UNIX_EPOCH
        + Duration::from_secs(parse_rfc3339(MOD_TIME).expect("time").unix_seconds as u64)
        + Duration::from_nanos(123_456_789);
    File::open(&fixture.archive)
        .expect("archive")
        .set_times(fs::FileTimes::new().set_modified(modified))
        .expect("nanosecond archive mtime");

    recover_selected_version_as_copy(fixture.request("precision (Recovered Copy).txt", &control))
        .expect("pinned upstream metadata is second precision");
}

#[test]
fn destination_collision_never_replaces_incumbent() {
    let fixture = Fixture::new("collision.txt", b"archive", b"current");
    let control = JobControl::new();
    let destination = fixture.destination("collision (Recovered Copy).txt");
    fs::write(&destination, b"incumbent").expect("collision fixture");

    assert_eq!(
        recover_selected_version_as_copy(
            fixture.request("collision (Recovered Copy).txt", &control)
        ),
        Err(RecoveryError::DestinationExists)
    );
    assert_eq!(fs::read(destination).expect("incumbent"), b"incumbent");
    assert_eq!(fs::read(&fixture.archive).expect("archive"), b"archive");
}

#[test]
fn changed_held_source_is_rejected_and_incomplete_copy_is_removed() {
    let archived = b"same length archive bytes\n";
    let fixture = Fixture::new("changed.txt", archived, b"current");
    let control = JobControl::new();
    let archive = fixture.archive.clone();
    let destination = fixture.destination("changed (Recovered Copy).txt");

    let result = recover_with_hooks(
        fixture.request("changed (Recovered Copy).txt", &control),
        &mut || {
            let mut changed = OpenOptions::new()
                .write(true)
                .open(&archive)
                .expect("archive writer");
            changed
                .write_all(b"same length changed bytes\n")
                .expect("change archive");
            changed.sync_all().expect("sync archive");
            Ok(())
        },
        &mut || Ok(()),
        &mut || {},
    );

    assert_eq!(result, Err(RecoveryError::SourceChanged));
    assert!(
        !destination.exists(),
        "only our incomplete inode was removed"
    );
}

#[test]
fn archive_path_symlink_substitution_is_rejected_without_touching_copy_name() {
    let fixture = Fixture::new("substitute.txt", b"archive", b"current");
    let control = JobControl::new();
    let archive = fixture.archive.clone();
    let outside = fixture.temporary.path().join("outside");
    let destination = fixture.destination("substitute (Recovered Copy).txt");
    fs::write(&outside, b"outside").expect("outside data");

    let result = recover_with_hooks(
        fixture.request("substitute (Recovered Copy).txt", &control),
        &mut || {
            fs::remove_file(&archive).expect("remove archive name");
            std::os::unix::fs::symlink(&outside, &archive).expect("substitute symlink");
            Ok(())
        },
        &mut || Ok(()),
        &mut || {},
    );

    assert_eq!(result, Err(RecoveryError::SourceChanged));
    assert!(!destination.exists());
    assert_eq!(fs::read(outside).expect("outside"), b"outside");
}

#[test]
fn archive_fifo_is_rejected_without_waiting_for_a_writer() {
    use std::process::Command;

    let fixture = Fixture::new("fifo.txt", b"archive", b"current");
    let control = JobControl::new();
    let destination = fixture.destination("fifo (Recovered Copy).txt");
    fs::remove_file(&fixture.archive).expect("replace archive with fifo");
    assert!(
        Command::new("/usr/bin/mkfifo")
            .arg(&fixture.archive)
            .status()
            .expect("run mkfifo")
            .success(),
        "create fifo"
    );

    let started = Instant::now();
    let result =
        recover_selected_version_as_copy(fixture.request("fifo (Recovered Copy).txt", &control));
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "FIFO open stayed bounded"
    );
    assert_eq!(result, Err(RecoveryError::SourceChanged));
    assert!(!destination.exists());
}

#[test]
fn post_sync_path_disappearance_or_replacement_is_preserved_as_uncertain() {
    let fixture = Fixture::new("post-sync.txt", b"archive", b"current");
    let control = JobControl::new();
    let disappeared = fixture.destination("disappeared (Recovered Copy).txt");
    let result = recover_with_hooks(
        fixture.request("disappeared (Recovered Copy).txt", &control),
        &mut || Ok(()),
        &mut || Ok(()),
        &mut || fs::remove_file(&disappeared).expect("remove synced path"),
    );
    assert_eq!(result, Err(RecoveryError::PersistenceUncertain));
    assert!(!disappeared.exists());

    let control = JobControl::new();
    let replacement = fixture.destination("replacement (Recovered Copy).txt");
    let replacement_path = replacement.clone();
    let result = recover_with_hooks(
        fixture.request("replacement (Recovered Copy).txt", &control),
        &mut || Ok(()),
        &mut || Ok(()),
        &mut || {
            fs::remove_file(&replacement_path).expect("remove synced inode");
            fs::write(&replacement_path, b"incumbent after sync").expect("replace synced path");
        },
    );
    assert_eq!(result, Err(RecoveryError::PersistenceUncertain));
    assert_eq!(
        fs::read(replacement).expect("replacement survives"),
        b"incumbent after sync"
    );
}

#[test]
fn mismatched_size_and_invalid_mapping_are_rejected_before_copy_creation() {
    let fixture = Fixture::new("safe.txt", b"archive", b"current");
    let control = JobControl::new();
    let destination = fixture.destination("safe (Recovered Copy).txt");
    let wrong_size = SelectedVersion::from_complete_listing(
        "safe.txt",
        &[VersionMetadata {
            version_time: VERSION_TIME.to_owned(),
            mod_time: MOD_TIME.to_owned(),
            size: 8,
        }],
        0,
    )
    .expect("metadata selection");
    let request = RecoverAsCopyRequest {
        selected: &wrong_size,
        ..fixture.request("safe (Recovered Copy).txt", &control)
    };
    assert_eq!(
        recover_selected_version_as_copy(request),
        Err(RecoveryError::SourceChanged)
    );
    assert!(!destination.exists());

    for path in ["../escape", ".stversions/file", "a//b", "a/\u{0001}"] {
        assert_eq!(
            SelectedVersion::from_complete_listing(
                path,
                &[VersionMetadata {
                    version_time: VERSION_TIME.to_owned(),
                    mod_time: MOD_TIME.to_owned(),
                    size: 7,
                }],
                0,
            ),
            Err(RecoveryError::UnsafeInput)
        );
    }
    assert_eq!(
        SelectedVersion::from_complete_listing(
            "safe.txt",
            &[VersionMetadata {
                version_time: "not a time".to_owned(),
                mod_time: MOD_TIME.to_owned(),
                size: 7,
            }],
            0,
        ),
        Err(RecoveryError::UnsafeInput)
    );
    assert_eq!(
        SelectedVersion::from_complete_listing(
            "safe.txt",
            &[VersionMetadata {
                version_time: VERSION_TIME.to_owned(),
                mod_time: "2026-09-08T03:57:54.1-04:00".to_owned(),
                size: 7,
            }],
            0,
        ),
        Err(RecoveryError::UnsafeInput)
    );
    assert_eq!(
        SelectedVersion::from_complete_listing(
            "safe.txt",
            &[
                VersionMetadata {
                    version_time: VERSION_TIME.to_owned(),
                    mod_time: MOD_TIME.to_owned(),
                    size: 7,
                },
                VersionMetadata {
                    version_time: VERSION_TIME.to_owned(),
                    mod_time: MOD_TIME.to_owned(),
                    size: 8,
                },
            ],
            0,
        ),
        Err(RecoveryError::UnsafeInput)
    );
    let request = RecoverAsCopyRequest {
        versioner: SimpleVersionerConfig {
            keep: 99,
            ..SimpleVersionerConfig::pinned_default()
        },
        ..fixture.request("safe (Recovered Copy).txt", &control)
    };
    assert_eq!(
        recover_selected_version_as_copy(request),
        Err(RecoveryError::UnsupportedVersioner)
    );
}

#[test]
fn cancellation_and_injected_io_failure_remove_only_our_owned_inode() {
    let archived = vec![0x5A; COPY_BUFFER_BYTES * 2 + 1];
    let fixture = Fixture::new("cancel.bin", &archived, b"current");
    let control = JobControl::new();
    let cancelling = control.clone();
    let cancelled_destination = fixture.destination("cancel (Recovered Copy).bin");
    let result = recover_with_hooks(
        fixture.request("cancel (Recovered Copy).bin", &control),
        &mut || Ok(()),
        &mut || {
            cancelling.cancel();
            Ok(())
        },
        &mut || {},
    );
    assert_eq!(result, Err(RecoveryError::Cancelled));
    assert!(!cancelled_destination.exists());

    let control = JobControl::new();
    let io_destination = fixture.destination("io (Recovered Copy).bin");
    let result = recover_with_hooks(
        fixture.request("io (Recovered Copy).bin", &control),
        &mut || Ok(()),
        &mut || Err(RecoveryError::IoFailure),
        &mut || {},
    );
    assert_eq!(result, Err(RecoveryError::IoFailure));
    assert!(!io_destination.exists());

    let control = JobControl::new();
    let replacement_destination = fixture.destination("replacement (Recovered Copy).bin");
    let attacker_destination = replacement_destination.clone();
    let result = recover_with_hooks(
        fixture.request("replacement (Recovered Copy).bin", &control),
        &mut || Ok(()),
        &mut || {
            fs::remove_file(&attacker_destination).expect("remove owned inode");
            fs::write(&attacker_destination, b"incumbent replacement")
                .expect("attacker replacement");
            Err(RecoveryError::IoFailure)
        },
        &mut || {},
    );
    assert_eq!(result, Err(RecoveryError::PersistenceUncertain));
    assert_eq!(
        fs::read(replacement_destination).expect("replacement"),
        b"incumbent replacement"
    );
}

#[test]
fn streams_a_bounded_buffer_across_multiple_chunks() {
    let archived = vec![0xA5; COPY_BUFFER_BYTES * 3 + 17];
    let fixture = Fixture::new("large.bin", &archived, b"current");
    let control = JobControl::new();
    let chunks = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&chunks);
    let receipt = recover_with_hooks(
        fixture.request("large (Recovered Copy).bin", &control),
        &mut || Ok(()),
        &mut || {
            observed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        },
        &mut || {},
    )
    .expect("streamed recovery");
    assert_eq!(chunks.load(Ordering::SeqCst), 4);
    assert_eq!(receipt.size, archived.len() as u64);
    assert_eq!(
        fs::read(fixture.destination("large (Recovered Copy).bin")).expect("copy"),
        archived
    );
}
