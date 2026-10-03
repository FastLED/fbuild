//! Once-per-install exec-bit repair for APE (cosmocc) host tools.
//!
//! Zip entries without Unix attributes extract as 0644. Fresh installs mark
//! APE tools executable during [`crate::PackageBase::staged_install`], but an
//! install extracted by an older fbuild can still hold a 0644 `tool.com`.
//! A marker file next to the `.install_complete` sentinel records that the
//! tree has been scanned, so every install pays for at most one scan, ever.

use crate::extractor;
use std::path::Path;

/// Marker written into an install dir once its APE tools are known executable.
/// Bump the suffix to force one more scan of every existing install.
const APE_PERMS_MARKER: &str = ".fbuild-ape-perms-v1";

/// Whether `installed_dir` has already been scanned for APE exec bits.
pub(crate) fn is_repaired(installed_dir: &Path) -> bool {
    installed_dir.join(APE_PERMS_MARKER).exists()
}

/// Mark APE tools executable in a freshly extracted tree and record the scan,
/// so the committed install never needs a repair pass.
pub(crate) fn mark_fresh(dir: &Path) -> fbuild_core::Result<()> {
    extractor::mark_ape_executables(dir)?;
    write_marker(dir);
    Ok(())
}

/// Repair an existing install's APE exec bits unless it was already scanned.
/// Best-effort: a failed scan is logged and retried on the next install hit.
/// Callers hold the package's install lock.
pub(crate) fn repair_once(installed_dir: &Path) {
    if is_repaired(installed_dir) {
        return;
    }
    match extractor::mark_ape_executables(installed_dir) {
        Ok(marked) => {
            if marked > 0 {
                tracing::info!(
                    "marked {marked} APE tool(s) executable in {}",
                    installed_dir.display()
                );
            }
            write_marker(installed_dir);
        }
        Err(e) => tracing::warn!(
            "failed to repair APE permissions in {}: {e}",
            installed_dir.display()
        ),
    }
}

fn write_marker(dir: &Path) {
    if let Err(e) = std::fs::write(dir.join(APE_PERMS_MARKER), b"") {
        tracing::warn!("failed to write APE permission marker: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CacheSubdir, PackageBase};

    const APE_BYTES: &[u8] = b"MZqFpD='\n#!/bin/sh\n";

    fn is_executable(path: &Path) -> bool {
        fbuild_core::platform::fs::is_executable(&std::fs::metadata(path).unwrap())
    }

    /// Recreate the file so it carries no exec bit (fresh files never do).
    fn clear_exec_bits(path: &Path) {
        let bytes = std::fs::read(path).unwrap();
        std::fs::remove_file(path).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn mark_fresh_writes_marker() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("tool.com"), APE_BYTES).unwrap();
        mark_fresh(tmp.path()).unwrap();
        assert!(is_repaired(tmp.path()));
        assert!(is_executable(&tmp.path().join("tool.com")));
    }

    /// The repair runs once: after the marker is written, a 0644 APE is no
    /// longer rescanned (it stays non-executable), proving the scan is not
    /// repeated on every install hit.
    #[test]
    fn repair_once_scans_only_once() {
        if fbuild_core::platform::host::current().is_windows() {
            return;
        }
        let tmp = tempfile::TempDir::new().unwrap();
        let tool = tmp.path().join("bin").join("tool.com");
        std::fs::create_dir_all(tool.parent().unwrap()).unwrap();
        std::fs::write(&tool, APE_BYTES).unwrap();
        clear_exec_bits(&tool);

        repair_once(tmp.path());
        assert!(
            is_executable(&tool),
            "first repair marks the APE executable"
        );
        assert!(is_repaired(tmp.path()));

        clear_exec_bits(&tool);
        repair_once(tmp.path());
        assert!(!is_executable(&tool), "a marked install is never rescanned");
    }

    /// CodeRabbit on FastLED/fbuild#1633: an install extracted before APE
    /// support (0644 `tool.com`, sentinel present, no marker) is repaired by
    /// the next `staged_install` hit, which never re-downloads.
    #[tokio::test]
    async fn staged_install_repairs_legacy_install_once() {
        if fbuild_core::platform::host::current().is_windows() {
            return;
        }
        let tmp = tempfile::TempDir::new().unwrap();
        let base = PackageBase::with_cache_root(
            "legacy-ape",
            "1.0",
            "http://127.0.0.1:9/unreachable.zip",
            "legacy-ape",
            None,
            CacheSubdir::Toolchains,
            tmp.path(),
            &tmp.path().join("cache"),
        );
        let install_path = base.install_path();
        let tool = install_path.join("bin").join("tool.com");
        std::fs::create_dir_all(tool.parent().unwrap()).unwrap();
        std::fs::write(&tool, APE_BYTES).unwrap();
        clear_exec_bits(&tool);
        std::fs::write(
            crate::disk_cache::paths::install_complete_sentinel(&install_path),
            b"",
        )
        .unwrap();

        let installed = base.staged_install(|_| Ok(())).await.unwrap();
        assert_eq!(installed, install_path);
        assert!(is_executable(&tool), "legacy 0644 APE must be repaired");
        assert!(is_repaired(&install_path));

        // Already-marked installs take the lock-free fast path untouched.
        clear_exec_bits(&tool);
        base.staged_install(|_| Ok(())).await.unwrap();
        assert!(!is_executable(&tool), "marked install is not rescanned");
    }
}
