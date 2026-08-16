#[cfg(not(unix))]
use std::fs::OpenOptions;
use std::fs::{self, File};
use std::io::{ErrorKind, Read, Write};
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::CoreError;

const RECORD_LOG_MAGIC: &[u8; 5] = b"CVWL\x01";
const RECORD_CHECKSUM_BYTES: usize = 32;

pub(crate) fn write_json_atomic(
    path: &Path,
    value: &impl Serialize,
    private: bool,
) -> Result<(), CoreError> {
    let bytes = serde_json::to_vec_pretty(value)?;
    write_atomic(path, &bytes, private)
}

pub(crate) fn read_json_bounded<T: DeserializeOwned>(
    path: &Path,
    maximum: usize,
) -> Result<T, CoreError> {
    let bytes = read_bounded(path, maximum)?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub(crate) fn read_bounded(path: &Path, maximum: usize) -> Result<Vec<u8>, CoreError> {
    #[cfg(unix)]
    let (metadata, file) = {
        use rustix::fs::{FileType, Mode, OFlags, fstat, open};

        let descriptor = open(
            path,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|error| CoreError::Io {
            operation: "open bounded file without following links",
            path: path.to_path_buf(),
            source: std::io::Error::from_raw_os_error(error.raw_os_error()),
        })?;
        let stat = fstat(&descriptor).map_err(|error| CoreError::Io {
            operation: "inspect bounded file handle",
            path: path.to_path_buf(),
            source: std::io::Error::from_raw_os_error(error.raw_os_error()),
        })?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
            return Err(CoreError::InvalidState(
                "bounded input is not a regular file".to_owned(),
            ));
        }
        let file = File::from(descriptor);
        let metadata = file.metadata().map_err(|source| CoreError::Io {
            operation: "inspect bounded file handle",
            path: path.to_path_buf(),
            source,
        })?;
        (metadata, file)
    };
    #[cfg(not(unix))]
    let (metadata, file) = {
        let metadata = fs::metadata(path).map_err(|source| CoreError::Io {
            operation: "inspect bounded file",
            path: path.to_path_buf(),
            source,
        })?;
        let file = File::open(path).map_err(|source| CoreError::Io {
            operation: "open bounded file",
            path: path.to_path_buf(),
            source,
        })?;
        (metadata, file)
    };
    if metadata.len() > maximum as u64 {
        return Err(CoreError::ResourceLimit("persisted file size"));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| CoreError::Io {
            operation: "read bounded file",
            path: path.to_path_buf(),
            source,
        })?;
    if bytes.len() > maximum {
        return Err(CoreError::ResourceLimit("persisted file size"));
    }
    Ok(bytes)
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8], private: bool) -> Result<(), CoreError> {
    let parent = path
        .parent()
        .ok_or_else(|| CoreError::InvalidState("atomic target has no parent".to_owned()))?;
    fs::create_dir_all(parent).map_err(|source| CoreError::Io {
        operation: "create atomic parent",
        path: parent.to_path_buf(),
        source,
    })?;

    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|source| CoreError::Io {
            operation: "create atomic staging file",
            path: parent.to_path_buf(),
            source,
        })?;
    temporary
        .write_all(bytes)
        .and_then(|()| temporary.flush())
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|source| CoreError::Io {
            operation: "sync atomic staging file",
            path: path.to_path_buf(),
            source,
        })?;

    #[cfg(unix)]
    if private {
        use std::os::unix::fs::PermissionsExt;

        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|source| CoreError::Io {
                operation: "protect atomic staging file",
                path: path.to_path_buf(),
                source,
            })?;
    }

    temporary.persist(path).map_err(|error| CoreError::Io {
        operation: "commit atomic file",
        path: path.to_path_buf(),
        source: error.error,
    })?;
    sync_directory(parent)
}

/// Atomically creates one durable file without ever replacing an incumbent.
pub(crate) fn write_atomic_noclobber(
    path: &Path,
    bytes: &[u8],
    private: bool,
) -> Result<bool, CoreError> {
    let parent = path
        .parent()
        .ok_or_else(|| CoreError::InvalidState("atomic target has no parent".to_owned()))?;
    fs::create_dir_all(parent).map_err(|source| CoreError::Io {
        operation: "create atomic parent",
        path: parent.to_path_buf(),
        source,
    })?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|source| CoreError::Io {
            operation: "create atomic staging file",
            path: parent.to_path_buf(),
            source,
        })?;
    temporary
        .write_all(bytes)
        .and_then(|()| temporary.flush())
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|source| CoreError::Io {
            operation: "sync atomic staging file",
            path: path.to_path_buf(),
            source,
        })?;
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|source| CoreError::Io {
                operation: "protect atomic staging file",
                path: path.to_path_buf(),
                source,
            })?;
    }
    match temporary.persist_noclobber(path) {
        Ok(_) => {
            sync_directory(parent)?;
            Ok(true)
        }
        Err(error) if error.error.kind() == ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(CoreError::Io {
            operation: "commit immutable atomic file",
            path: path.to_path_buf(),
            source: error.error,
        }),
    }
}

pub(crate) fn append_record_log(
    path: &Path,
    payload: &[u8],
    maximum_record_bytes: usize,
    maximum_log_bytes: u64,
    private: bool,
    durable: bool,
) -> Result<(), CoreError> {
    if payload.is_empty()
        || payload.len() > maximum_record_bytes
        || payload.len() > u32::MAX as usize
    {
        return Err(CoreError::ResourceLimit("checkpoint record"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| CoreError::InvalidState("record log has no parent".to_owned()))?;
    fs::create_dir_all(parent).map_err(|source| CoreError::Io {
        operation: "create record log parent",
        path: parent.to_path_buf(),
        source,
    })?;
    let existed = path.exists();
    let mut file = open_record_log(path, true)?;
    let metadata = file.metadata().map_err(|source| CoreError::Io {
        operation: "inspect record log",
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(CoreError::InvalidState(
            "record log is not a regular file".to_owned(),
        ));
    }
    let prefix_bytes = if metadata.len() == 0 {
        RECORD_LOG_MAGIC.len() as u64
    } else {
        if metadata.len() < RECORD_LOG_MAGIC.len() as u64 {
            return Err(CoreError::InvalidState(
                "truncated record log header".to_owned(),
            ));
        }
        0
    };
    let frame_bytes = 4_u64
        .saturating_add(payload.len() as u64)
        .saturating_add(RECORD_CHECKSUM_BYTES as u64);
    if metadata
        .len()
        .saturating_add(prefix_bytes)
        .saturating_add(frame_bytes)
        > maximum_log_bytes
    {
        return Err(CoreError::ResourceLimit("job checkpoint log"));
    }
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|source| CoreError::Io {
                operation: "protect record log",
                path: path.to_path_buf(),
                source,
            })?;
    }
    if metadata.len() == 0 {
        file.write_all(RECORD_LOG_MAGIC)
            .map_err(|source| CoreError::Io {
                operation: "write record log header",
                path: path.to_path_buf(),
                source,
            })?;
    }
    write_record_frame(&mut file, path, payload)?;
    if durable {
        file.sync_all().map_err(|source| CoreError::Io {
            operation: "sync record log",
            path: path.to_path_buf(),
            source,
        })?;
    }
    if !existed && durable {
        sync_directory(parent)?;
    }
    Ok(())
}

pub(crate) fn sync_record_log(path: &Path) -> Result<(), CoreError> {
    let file = open_record_log(path, false)?;
    file.sync_all().map_err(|source| CoreError::Io {
        operation: "sync record log",
        path: path.to_path_buf(),
        source,
    })
}

pub(crate) fn read_record_log(
    path: &Path,
    maximum_record_bytes: usize,
    maximum_log_bytes: u64,
) -> Result<Option<Vec<Vec<u8>>>, CoreError> {
    let mut file = match open_record_log(path, false) {
        Ok(file) => file,
        Err(CoreError::Io { source, .. }) if source.kind() == ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let metadata = file.metadata().map_err(|source| CoreError::Io {
        operation: "inspect record log",
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_file() || metadata.len() > maximum_log_bytes {
        return Err(CoreError::ResourceLimit("job checkpoint log"));
    }
    let mut magic = [0_u8; RECORD_LOG_MAGIC.len()];
    file.read_exact(&mut magic)
        .map_err(|source| CoreError::Io {
            operation: "read record log header",
            path: path.to_path_buf(),
            source,
        })?;
    if &magic != RECORD_LOG_MAGIC {
        return Err(CoreError::InvalidState(
            "invalid record log header".to_owned(),
        ));
    }
    let mut records = Vec::new();
    let mut valid_length = RECORD_LOG_MAGIC.len() as u64;
    loop {
        let mut length_bytes = [0_u8; 4];
        let mut length_read = 0;
        while length_read < length_bytes.len() {
            match file.read(&mut length_bytes[length_read..]) {
                Ok(0) => break,
                Ok(count) => length_read += count,
                Err(source) => {
                    return Err(CoreError::Io {
                        operation: "read record length",
                        path: path.to_path_buf(),
                        source,
                    });
                }
            }
        }
        if length_read == 0 {
            break;
        }
        if length_read < length_bytes.len() {
            truncate_record_log_tail(&file, path, valid_length)?;
            break;
        }
        let length = u32::from_be_bytes(length_bytes) as usize;
        if length == 0 || length > maximum_record_bytes {
            return Err(CoreError::InvalidState(
                "invalid checkpoint record length".to_owned(),
            ));
        }
        let mut payload = vec![0_u8; length];
        let mut checksum = [0_u8; RECORD_CHECKSUM_BYTES];
        if let Err(source) = file
            .read_exact(&mut payload)
            .and_then(|()| file.read_exact(&mut checksum))
        {
            if source.kind() == ErrorKind::UnexpectedEof {
                truncate_record_log_tail(&file, path, valid_length)?;
                break;
            }
            return Err(CoreError::Io {
                operation: "read checkpoint record",
                path: path.to_path_buf(),
                source,
            });
        }
        if checksum != record_checksum(&length_bytes, &payload) {
            let frame_end = valid_length
                .saturating_add(4)
                .saturating_add(length as u64)
                .saturating_add(RECORD_CHECKSUM_BYTES as u64);
            if frame_end == metadata.len() {
                truncate_record_log_tail(&file, path, valid_length)?;
                break;
            }
            return Err(CoreError::InvalidState(
                "checkpoint record checksum mismatch".to_owned(),
            ));
        }
        valid_length = valid_length
            .saturating_add(4)
            .saturating_add(length as u64)
            .saturating_add(RECORD_CHECKSUM_BYTES as u64);
        records.push(payload);
    }
    Ok(Some(records))
}

pub(crate) fn rewrite_record_log(
    path: &Path,
    records: &[Vec<u8>],
    maximum_record_bytes: usize,
    maximum_log_bytes: u64,
    private: bool,
) -> Result<(), CoreError> {
    let parent = path
        .parent()
        .ok_or_else(|| CoreError::InvalidState("record log has no parent".to_owned()))?;
    fs::create_dir_all(parent).map_err(|source| CoreError::Io {
        operation: "create record log parent",
        path: parent.to_path_buf(),
        source,
    })?;
    let mut total = RECORD_LOG_MAGIC.len() as u64;
    for record in records {
        if record.is_empty() || record.len() > maximum_record_bytes {
            return Err(CoreError::ResourceLimit("checkpoint record"));
        }
        total = total
            .saturating_add(4)
            .saturating_add(record.len() as u64)
            .saturating_add(RECORD_CHECKSUM_BYTES as u64);
    }
    if total > maximum_log_bytes {
        return Err(CoreError::ResourceLimit("job checkpoint log"));
    }
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|source| CoreError::Io {
            operation: "create record log staging file",
            path: parent.to_path_buf(),
            source,
        })?;
    temporary
        .write_all(RECORD_LOG_MAGIC)
        .map_err(|source| CoreError::Io {
            operation: "write record log header",
            path: path.to_path_buf(),
            source,
        })?;
    for record in records {
        write_record_frame(&mut temporary, path, record)?;
    }
    temporary
        .flush()
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|source| CoreError::Io {
            operation: "sync record log staging file",
            path: path.to_path_buf(),
            source,
        })?;
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|source| CoreError::Io {
                operation: "protect record log staging file",
                path: path.to_path_buf(),
                source,
            })?;
    }
    temporary.persist(path).map_err(|error| CoreError::Io {
        operation: "commit record log",
        path: path.to_path_buf(),
        source: error.error,
    })?;
    sync_directory(parent)
}

fn write_record_frame(file: &mut impl Write, path: &Path, payload: &[u8]) -> Result<(), CoreError> {
    let length = u32::try_from(payload.len())
        .map_err(|_| CoreError::ResourceLimit("checkpoint record"))?
        .to_be_bytes();
    let checksum = record_checksum(&length, payload);
    file.write_all(&length)
        .and_then(|()| file.write_all(payload))
        .and_then(|()| file.write_all(&checksum))
        .map_err(|source| CoreError::Io {
            operation: "append checkpoint record",
            path: path.to_path_buf(),
            source,
        })
}

fn record_checksum(length: &[u8; 4], payload: &[u8]) -> [u8; RECORD_CHECKSUM_BYTES] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"covalent/checkpoint-record/v1\0");
    hasher.update(length);
    hasher.update(payload);
    *hasher.finalize().as_bytes()
}

fn truncate_record_log_tail(file: &File, path: &Path, valid_length: u64) -> Result<(), CoreError> {
    file.set_len(valid_length)
        .and_then(|()| file.sync_all())
        .map_err(|source| CoreError::Io {
            operation: "recover partial checkpoint record",
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(unix)]
fn open_record_log(path: &Path, append: bool) -> Result<File, CoreError> {
    use rustix::fs::{Mode, OFlags, open};
    let mut flags = OFlags::CLOEXEC | OFlags::NOFOLLOW;
    flags |= if append {
        OFlags::CREATE | OFlags::WRONLY | OFlags::APPEND
    } else {
        OFlags::RDWR
    };
    let descriptor = open(path, flags, Mode::RUSR | Mode::WUSR).map_err(|error| CoreError::Io {
        operation: "open checkpoint record log",
        path: path.to_path_buf(),
        source: std::io::Error::from_raw_os_error(error.raw_os_error()),
    })?;
    Ok(File::from(descriptor))
}

#[cfg(not(unix))]
fn open_record_log(path: &Path, append: bool) -> Result<File, CoreError> {
    let mut options = OpenOptions::new();
    options
        .read(!append)
        .write(true)
        .create(append)
        .append(append);
    options.open(path).map_err(|source| CoreError::Io {
        operation: "open checkpoint record log",
        path: path.to_path_buf(),
        source,
    })
}

pub(crate) fn sync_directory(path: &Path) -> Result<(), CoreError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| CoreError::Io {
            operation: "sync parent directory",
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Seek as _, SeekFrom, Write as _};

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn atomic_noclobber_never_replaces_an_incumbent() {
        let directory = tempdir().expect("directory");
        let path = directory.path().join("immutable");
        assert!(write_atomic_noclobber(&path, b"first", true).expect("first write"));
        assert!(!write_atomic_noclobber(&path, b"second", true).expect("conflict"));
        assert_eq!(fs::read(path).expect("incumbent"), b"first");
    }

    #[test]
    fn record_log_recovers_only_the_torn_trailing_frame() {
        let directory = tempdir().expect("directory");
        let path = directory.path().join("checkpoint.wal");
        append_record_log(&path, b"first", 128, 4_096, true, true).expect("first");
        let valid_length = fs::metadata(&path).expect("metadata").len();
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open append");
        file.write_all(&10_u32.to_be_bytes()[..2])
            .expect("partial length");
        file.sync_all().expect("sync partial");

        assert_eq!(
            read_record_log(&path, 128, 4_096).expect("recover"),
            Some(vec![b"first".to_vec()])
        );
        assert_eq!(fs::metadata(&path).expect("metadata").len(), valid_length);
    }

    #[test]
    fn record_log_discards_a_torn_final_checksum_but_rejects_middle_corruption() {
        let directory = tempdir().expect("directory");
        let path = directory.path().join("checkpoint.wal");
        append_record_log(&path, b"first", 128, 4_096, true, true).expect("first");
        let first_length = fs::metadata(&path).expect("metadata").len();
        append_record_log(&path, b"second", 128, 4_096, true, true).expect("second");
        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("open corrupt");
        file.seek(SeekFrom::End(-1)).expect("seek checksum");
        let mut byte = [0_u8; 1];
        file.read_exact(&mut byte).expect("read checksum");
        byte[0] ^= 0xff;
        file.seek(SeekFrom::End(-1)).expect("reseak checksum");
        file.write_all(&byte).expect("corrupt checksum");
        file.sync_all().expect("sync corruption");
        assert_eq!(
            read_record_log(&path, 128, 4_096).expect("recover final"),
            Some(vec![b"first".to_vec()])
        );
        assert_eq!(fs::metadata(&path).expect("metadata").len(), first_length);

        append_record_log(&path, b"third", 128, 4_096, true, true).expect("third");
        append_record_log(&path, b"fourth", 128, 4_096, true, true).expect("fourth");
        let first_checksum_offset = RECORD_LOG_MAGIC.len() as u64 + 4 + b"first".len() as u64;
        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("open corrupt middle");
        file.seek(SeekFrom::Start(first_checksum_offset))
            .expect("seek middle checksum");
        file.read_exact(&mut byte).expect("read middle checksum");
        byte[0] ^= 0xff;
        file.seek(SeekFrom::Start(first_checksum_offset))
            .expect("reseak middle checksum");
        file.write_all(&byte).expect("corrupt middle checksum");
        file.sync_all().expect("sync corruption");
        assert!(read_record_log(&path, 128, 4_096).is_err());
    }
}
