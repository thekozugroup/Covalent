use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::CoreError;

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

pub(crate) fn sync_directory(path: &Path) -> Result<(), CoreError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| CoreError::Io {
            operation: "sync parent directory",
            path: path.to_path_buf(),
            source,
        })
}
