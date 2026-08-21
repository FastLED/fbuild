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

impl Drop for FileLockGuard {
    fn drop(&mut self) {
        // Unlock explicitly instead of relying on close: macOS can transiently
        // report contention for a re-acquire racing the close-release
        // (FastLED/fbuild#1340). Explicit LOCK_UN is the deterministic release;
        // soldr's lifecycle guard unlocks the same way.
        let _ = self._file.unlock();
    }
}

/// Try to acquire an OS-released lock on `path`.
///
/// Returns `Ok(None)` when another process holds a conflicting lock. The lock
/// is released automatically when the guard is dropped or the process exits.
pub fn try_acquire(path: &Path, mode: FileLockMode) -> io::Result<Option<FileLockGuard>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = retry_on_interrupt(|| {
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
    })?;
    let result = retry_on_interrupt(|| match mode {
        FileLockMode::Shared => FileExt::try_lock_shared(&file),
        FileLockMode::Exclusive => FileExt::try_lock_exclusive(&file),
    });
    match result {
        Ok(()) => Ok(Some(FileLockGuard { _file: file })),
        Err(error) if lock_is_held(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

/// How many times an `EINTR` is retried before giving up. `EINTR` means a
/// signal arrived, not that anything is wrong; a handful of retries covers a
/// burst without spinning forever if something is delivering signals
/// continuously.
const INTERRUPT_RETRIES: usize = 8;

/// Retry `op` while it fails with `ErrorKind::Interrupted`.
///
/// `flock()` is interruptible by signals on macOS/BSD, and `fs2` returns the
/// raw OS error, so an `EINTR` propagates out as a hard error — which callers
/// that collapse errors into "lock unavailable" then report as routine
/// contention. `open()` is interruptible for the same reason. Neither is a
/// real failure, so neither should surface as one (FastLED/fbuild#1222).
fn retry_on_interrupt<T>(mut op: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let mut attempts = 0;
    loop {
        match op() {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                attempts += 1;
                if attempts >= INTERRUPT_RETRIES {
                    return Err(error);
                }
            }
            other => return other,
        }
    }
}

/// Does this error mean "another process holds a conflicting lock"?
///
/// Unix reports contention as `EWOULDBLOCK` (kind `WouldBlock`), but Windows
/// `LockFileEx(LOCKFILE_FAIL_IMMEDIATELY)` reports `ERROR_LOCK_VIOLATION`
/// (os error 33), which std maps to an uncategorized kind — so a kind check
/// alone misclassifies contention as a hard error on Windows. Also compare
/// against `fs2::lock_contended_error()` (the canonical per-platform
/// contention error), mirroring soldr's `lock_is_held`.
fn lock_is_held(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::WouldBlock
        || error.raw_os_error() == fs2::lock_contended_error().raw_os_error()
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

    /// Post-release re-acquires poll through a short deadline instead of
    /// asserting single-shot availability: macOS can transiently report
    /// contention just after close-release (FastLED/fbuild#1340). The
    /// production contract is poll-and-retry, so the property under test is
    /// eventual availability.
    fn reacquire_within(path: &Path, mode: FileLockMode) -> FileLockGuard {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if let Some(guard) = try_acquire(path, mode).unwrap() {
                return guard;
            }
            assert!(
                Instant::now() < deadline,
                "lock at {} did not become available within 1s of release",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

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
        let _third = reacquire_within(&path, FileLockMode::Exclusive);
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
        let _shared = reacquire_within(&path, FileLockMode::Shared);
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
