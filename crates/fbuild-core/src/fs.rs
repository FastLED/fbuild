//! Async filesystem bridge.
//!
//! FastLED/fbuild#844 (bridge sweep). All async filesystem access in
//! the workspace flows through this module so:
//!
//! 1. The workspace has **one** source of truth for `tokio::fs` —
//!    swapping the backend (e.g. an `io_uring` impl) is a one-file
//!    change.
//! 2. `std::fs::*` inside `async fn` is lint-banned
//!    (`ban_std_fs_in_async`) because blocking the tokio worker on
//!    filesystem I/O is a latency landmine.
//! 3. Direct `tokio::fs` imports are lint-banned
//!    (`ban_tokio_fs_direct_import`) so the curated surface here stays
//!    the canonical entry point.
//!
//! For `std::fs::*` in synchronous, non-`async` code paths (the
//! occasional `OnceLock` init, the synchronous main of a binary,
//! `#[cfg(test)]` blocks) the lint scope is narrow enough that direct
//! `std::fs` use is fine. The `spawn_blocking` escape hatch covers the
//! rare case where a synchronous filesystem call from inside async is
//! genuinely correct.
//!
//! ## Atomic writes
//!
//! [`write_atomic`] writes to a sibling temp path, `fsync`s, then
//! atomic-renames into place. Required for every state-file write that
//! corruption would block a rebuild (build fingerprint JSON, framework
//! discovery cache, etc.) — see FastLED/fbuild#844 "Bridge pair 6".

// Curated re-export of the `tokio::fs` surface fbuild uses. Adding new
// items here is preferable to having callers `use tokio::fs` directly —
// the matching `ban_tokio_fs_direct_import` dylint enforces this.
pub use tokio::fs::{
    canonicalize, copy, create_dir, create_dir_all, hard_link, metadata, read, read_dir,
    read_link, read_to_string, remove_dir, remove_dir_all, remove_file, rename,
    set_permissions, symlink_metadata, write, DirEntry, File, OpenOptions, ReadDir,
};

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// Process-monotonic nonce for atomic-write tempfile names. Combined
/// with the PID this guarantees uniqueness even if two threads race on
/// the same target path.
static WRITE_ATOMIC_NONCE: AtomicU64 = AtomicU64::new(0);

/// Write a file atomically. Writes to `<path>.tmp.<pid>.<nonce>`,
/// `fsync`s the file (and on POSIX the parent dir), then atomic-renames
/// into place.
///
/// On POSIX `rename(2)` is atomic on the same filesystem. On Windows
/// tokio's `rename` lowers to `MoveFileExW` with
/// `MOVEFILE_REPLACE_EXISTING`, which is atomic on NTFS/ReFS. If a
/// caller is writing across filesystems, atomicity degrades to "at
/// most one of the two files exists after the write" — still strictly
/// better than the corrupt-mid-write hazard of `tokio::fs::write`.
///
/// Use this for every state-file write that corruption would block a
/// rebuild — build fingerprint JSON, framework discovery cache,
/// clangd config, symbol caches, etc. See FastLED/fbuild#844
/// "Bridge pair 6" for the migration list.
pub async fn write_atomic(
    path: impl AsRef<Path>,
    content: impl AsRef<[u8]>,
) -> std::io::Result<()> {
    let path = path.as_ref();
    let nonce = WRITE_ATOMIC_NONCE.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();

    // Build the temp path: `<path>.tmp.<pid>.<nonce>`. Sibling of the
    // target so the rename stays on the same filesystem.
    let mut tmp_name = path
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_default();
    if tmp_name.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "write_atomic: target path has no file name",
        ));
    }
    tmp_name.push(format!(".tmp.{pid}.{nonce}"));
    let tmp_path = match path.parent() {
        Some(parent) => parent.join(&tmp_name),
        None => std::path::PathBuf::from(&tmp_name),
    };

    // Ensure parent dir exists. Mirrors `tokio::fs::write`'s contract
    // (which doesn't create parents) — leave that policy to the
    // caller, but fail loudly if parent is missing rather than silently
    // dropping the write into the void.
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            // Don't auto-create — if the parent is missing that's a
            // caller bug worth surfacing. Just probe.
            match tokio::fs::metadata(parent).await {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        format!(
                            "write_atomic: parent directory does not exist: {}",
                            parent.display()
                        ),
                    ));
                }
                Err(e) => return Err(e),
            }
        }
    }

    // Write + fsync the temp file via the async surface. `sync_all`
    // on tokio's File lowers to the OS sync (`fsync` / `FlushFileBuffers`)
    // through the tokio blocking pool — same syscall, just dispatched
    // off the reactor thread.
    {
        use tokio::io::AsyncWriteExt;
        let mut file = tokio::fs::File::create(&tmp_path).await?;
        let write_res = file.write_all(content.as_ref()).await;
        if let Err(e) = write_res {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return Err(e);
        }
        let sync_res = file.sync_all().await;
        if let Err(e) = sync_res {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return Err(e);
        }
        // Drop the handle before rename — Windows requires the file to
        // be closed (or opened FILE_SHARE_DELETE, which `tokio::fs`
        // does not request) for `MoveFileExW` to succeed.
        drop(file);
    }

    // Atomic rename. On NTFS this is `MoveFileExW(...,
    // MOVEFILE_REPLACE_EXISTING)`. On POSIX it's plain `rename(2)`.
    if let Err(e) = tokio::fs::rename(&tmp_path, path).await {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return Err(e);
    }

    Ok(())
}

/// Synchronous companion to [`write_atomic`] — same atomic-rename
/// semantics, but `std::fs` end-to-end so it's callable from a
/// `current-thread` tokio runtime without any block-on / `block_in_place`
/// gymnastics.
///
/// FastLED/fbuild#865: `StatusManager::write_atomic` previously bridged
/// to the async [`write_atomic`] via `block_in_place` + `Handle::block_on`.
/// `block_in_place` panics inside a current-thread runtime, which is the
/// default flavor of `#[tokio::test]`, so every unit test that touched
/// the status writer panicked on macOS + Windows CI.
///
/// Use this from any sync caller (the daemon's status writer, in-process
/// snapshots, test harnesses). Prefer [`write_atomic`] from async paths
/// that need to avoid blocking the reactor on the fsync.
pub fn write_atomic_sync(
    path: impl AsRef<Path>,
    content: impl AsRef<[u8]>,
) -> std::io::Result<()> {
    use std::io::Write as _;

    let path = path.as_ref();
    let nonce = WRITE_ATOMIC_NONCE.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();

    let mut tmp_name = path
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_default();
    if tmp_name.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "write_atomic_sync: target path has no file name",
        ));
    }
    tmp_name.push(format!(".tmp.{pid}.{nonce}"));
    let tmp_path = match path.parent() {
        Some(parent) => parent.join(&tmp_name),
        None => std::path::PathBuf::from(&tmp_name),
    };

    // Mirror `write_atomic`'s parent-must-exist contract.
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            match std::fs::metadata(parent) {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        format!(
                            "write_atomic_sync: parent directory does not exist: {}",
                            parent.display()
                        ),
                    ));
                }
                Err(e) => return Err(e),
            }
        }
    }

    {
        let mut file = std::fs::File::create(&tmp_path)?;
        if let Err(e) = file.write_all(content.as_ref()) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(e);
        }
        if let Err(e) = file.sync_all() {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(e);
        }
        // Drop before rename — same Windows MoveFileExW share-mode
        // constraint that `write_atomic` documents.
        drop(file);
    }

    if let Err(e) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn write_atomic_creates_file_with_expected_content() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("state.json");
        write_atomic(&target, b"{\"k\":1}").await.unwrap();
        let got = tokio::fs::read(&target).await.unwrap();
        assert_eq!(got, b"{\"k\":1}");
    }

    #[tokio::test]
    async fn write_atomic_overwrites_existing_file() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("state.json");
        tokio::fs::write(&target, b"old").await.unwrap();
        write_atomic(&target, b"new").await.unwrap();
        let got = tokio::fs::read(&target).await.unwrap();
        assert_eq!(got, b"new");
    }

    #[tokio::test]
    async fn write_atomic_does_not_leave_tempfile_on_success() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("state.json");
        write_atomic(&target, b"hello").await.unwrap();
        // No sibling `.tmp.<pid>.<nonce>` left behind.
        let mut entries = tokio::fs::read_dir(dir.path()).await.unwrap();
        let mut count = 0;
        while let Some(entry) = entries.next_entry().await.unwrap() {
            count += 1;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            assert!(
                !name.contains(".tmp."),
                "leftover tempfile: {name}"
            );
        }
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn write_atomic_errors_when_parent_missing() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("does_not_exist").join("state.json");
        let err = write_atomic(&target, b"x").await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

    // FastLED/fbuild#865 regression: `write_atomic_sync` must be safe to
    // call inside a current-thread tokio runtime (the default flavor of
    // `#[tokio::test]`) because that's the path the daemon's status
    // writer takes from unit tests. The previous async bridge panicked
    // on `block_in_place` here.
    #[tokio::test]
    async fn write_atomic_sync_runs_inside_current_thread_runtime() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("state.json");
        write_atomic_sync(&target, b"sync-payload").unwrap();
        let got = std::fs::read(&target).unwrap();
        assert_eq!(got, b"sync-payload");
    }

    #[test]
    fn write_atomic_sync_works_without_runtime() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("state.json");
        write_atomic_sync(&target, b"no-runtime").unwrap();
        let got = std::fs::read(&target).unwrap();
        assert_eq!(got, b"no-runtime");
    }

    #[test]
    fn write_atomic_sync_errors_when_parent_missing() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("does_not_exist").join("state.json");
        let err = write_atomic_sync(&target, b"x").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

    #[tokio::test]
    async fn write_atomic_handles_concurrent_writes() {
        // Two concurrent writes to the same target must each complete
        // successfully (last-writer-wins) and the file must end up
        // with one of the two payloads, never a torn write.
        let dir = tempdir().unwrap();
        let target = dir.path().join("state.json");
        let t1 = {
            let target = target.clone();
            tokio::spawn(async move { write_atomic(&target, b"AAAAA").await })
        };
        let t2 = {
            let target = target.clone();
            tokio::spawn(async move { write_atomic(&target, b"BBBBB").await })
        };
        t1.await.unwrap().unwrap();
        t2.await.unwrap().unwrap();
        let got = tokio::fs::read(&target).await.unwrap();
        assert!(got == b"AAAAA" || got == b"BBBBB", "torn write: {got:?}");
    }
}
