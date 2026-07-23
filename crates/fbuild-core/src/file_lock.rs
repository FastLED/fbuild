//! Cross-process locks for fbuild-daemon startup/lifecycle coordination.
//!
//! These locks are deliberately outside zccache's compile/object hot path,
//! whose synchronization remains internal to zccache.

use fs2::FileExt;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileLockMode {
    Shared,
    Exclusive,
}

#[derive(Debug)]
pub struct FileLockGuard {
    _file: File,
}

/// Try to acquire an OS-released lock on `path`.
///
/// Returns `Ok(None)` when another process holds a conflicting lock. The lock
/// is released automatically when the guard is dropped or the process exits.
pub fn try_acquire(path: &Path, mode: FileLockMode) -> io::Result<Option<FileLockGuard>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    let result = match mode {
        FileLockMode::Shared => FileExt::try_lock_shared(&file),
        FileLockMode::Exclusive => FileExt::try_lock_exclusive(&file),
    };
    match result {
        Ok(()) => Ok(Some(FileLockGuard { _file: file })),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(error),
    }
}

/// Wait up to `timeout` for a cross-process file lock.
pub async fn acquire(
    path: &Path,
    mode: FileLockMode,
    timeout: Duration,
    poll: Duration,
) -> io::Result<FileLockGuard> {
    let started = Instant::now();
    loop {
        if let Some(guard) = try_acquire(path, mode)? {
            return Ok(guard);
        }
        if started.elapsed() >= timeout {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "timed out after {:.1}s waiting for {} lock on {}",
                    timeout.as_secs_f64(),
                    match mode {
                        FileLockMode::Shared => "shared",
                        FileLockMode::Exclusive => "exclusive",
                    },
                    path.display()
                ),
            ));
        }
        tokio::time::sleep(poll).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_holders_block_exclusive_until_all_release() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cache.lock");
        let first = try_acquire(&path, FileLockMode::Shared)
            .unwrap()
            .expect("first shared lock");
        let second = try_acquire(&path, FileLockMode::Shared)
            .unwrap()
            .expect("second shared lock");

        assert!(
            try_acquire(&path, FileLockMode::Exclusive)
                .unwrap()
                .is_none()
        );
        drop(first);
        assert!(
            try_acquire(&path, FileLockMode::Exclusive)
                .unwrap()
                .is_none()
        );
        drop(second);
        assert!(
            try_acquire(&path, FileLockMode::Exclusive)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn exclusive_holder_blocks_shared_until_release() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cache.lock");
        let exclusive = try_acquire(&path, FileLockMode::Exclusive)
            .unwrap()
            .expect("exclusive lock");

        assert!(try_acquire(&path, FileLockMode::Shared).unwrap().is_none());
        drop(exclusive);
        assert!(try_acquire(&path, FileLockMode::Shared).unwrap().is_some());
    }

    #[tokio::test]
    async fn timed_acquire_fails_closed_while_conflicting_lock_is_held() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cache.lock");
        let _exclusive = try_acquire(&path, FileLockMode::Exclusive)
            .unwrap()
            .expect("exclusive lock");

        let error = acquire(
            &path,
            FileLockMode::Shared,
            Duration::from_millis(20),
            Duration::from_millis(5),
        )
        .await
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }
}
