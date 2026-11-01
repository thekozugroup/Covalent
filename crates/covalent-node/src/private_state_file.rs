//! Shared bounded journal IO. Never reset missing/damaged state behind an old lock.
use rustix::fs::{FileType, Mode, OFlags, fstat, open};
use std::{fs::File, io::Read as _, os::unix::fs::MetadataExt as _, path::PathBuf};

const MAX_BYTES: u64 = 128 * 1024;

pub(crate) struct PrivateStateFile {
    path: PathBuf,
    lock: File,
    #[cfg(test)]
    fail_after_publish: std::sync::atomic::AtomicBool,
}
impl PrivateStateFile {
    pub(crate) fn open(path: PathBuf) -> Result<(Self, Option<Vec<u8>>), ()> {
        Self::parent(&path)?;
        let lock_path = path.with_extension("lock");
        let (fd, fresh) = match open(
            &lock_path,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        ) {
            Ok(fd) => (fd, true),
            Err(rustix::io::Errno::EXIST) => (
                open(
                    &lock_path,
                    OFlags::WRONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(|_| ())?,
                false,
            ),
            Err(_) => return Err(()),
        };
        let stat = fstat(&fd).map_err(|_| ())?;
        Self::regular(&stat, 0)?;
        if stat.st_size != 0 {
            return Err(());
        }
        let lock = File::from(fd);
        fs2::FileExt::try_lock_exclusive(&lock).map_err(|_| ())?;
        let file = Self {
            path,
            lock,
            #[cfg(test)]
            fail_after_publish: std::sync::atomic::AtomicBool::new(false),
        };
        let bytes = match file.read() {
            Ok(bytes) => Some(bytes),
            Err(rustix::io::Errno::NOENT) if fresh => None,
            Err(_) => return Err(()),
        };
        file.check_lock()?;
        Ok((file, bytes))
    }
    fn parent(path: &std::path::Path) -> Result<(), ()> {
        let parent = path.parent().ok_or(())?;
        let meta = std::fs::symlink_metadata(parent).map_err(|_| ())?;
        if parent.canonicalize().map_err(|_| ())?.as_path() != parent
            || !meta.is_dir()
            || meta.uid() != rustix::process::geteuid().as_raw()
            || meta.mode() & 0o077 != 0
        {
            return Err(());
        }
        Ok(())
    }
    fn regular(stat: &rustix::fs::Stat, limit: u64) -> Result<(), ()> {
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
            || stat.st_uid != rustix::process::geteuid().as_raw()
            || stat.st_nlink != 1
            || stat.st_mode & 0o077 != 0
            || stat.st_size < 0
            || stat.st_size as u64 > limit
        {
            return Err(());
        }
        Ok(())
    }
    fn check_lock(&self) -> Result<(), ()> {
        Self::parent(&self.path)?;
        let fd = open(
            self.path.with_extension("lock"),
            OFlags::WRONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| ())?;
        let now = fstat(fd).map_err(|_| ())?;
        let held = fstat(&self.lock).map_err(|_| ())?;
        Self::regular(&now, 0)?;
        if held.st_dev != now.st_dev || held.st_ino != now.st_ino {
            return Err(());
        }
        Ok(())
    }
    fn read(&self) -> Result<Vec<u8>, rustix::io::Errno> {
        let fd = open(
            &self.path,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        let stat = fstat(&fd)?;
        Self::regular(&stat, MAX_BYTES).map_err(|_| rustix::io::Errno::INVAL)?;
        let mut bytes = Vec::new();
        File::from(fd)
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| rustix::io::Errno::IO)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(rustix::io::Errno::FBIG);
        }
        Ok(bytes)
    }
    pub(crate) fn matches(&self, expected: &[u8]) -> Result<(), ()> {
        self.check_lock()?;
        if self.read().map_err(|_| ())? != expected {
            return Err(());
        }
        self.check_lock()
    }
    pub(crate) fn persist(&self, bytes: &[u8]) -> Result<(), ()> {
        if bytes.len() as u64 > MAX_BYTES {
            return Err(());
        }
        self.check_lock()?;
        crate::persist_private_file(&self.path, bytes).map_err(|_| ())?;
        #[cfg(test)]
        if self
            .fail_after_publish
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(());
        }
        self.matches(bytes)
    }
    #[cfg(test)]
    pub(crate) fn fail_once_after_publish(&self) {
        self.fail_after_publish
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}
