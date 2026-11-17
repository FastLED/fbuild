//! Sentinel-aware install lookup and repair for [`PackageBase::staged_install`]
//! (FastLED/fbuild#2043).
//!
//! A directory at the final install path is only an install when it carries
//! the `.install_complete` sentinel, the same contract [`PackageBase::is_cached`]
//! enforces. Anything else there (a stray write, a CI cache restored without
//! the sentinel, a legacy partial tree) is foreign: staging always extracts
//! into a `_staging` sibling and commits with a rename under the install lock,
//! so a sentinel-less final path is never a concurrent install in progress
//! once that lock is held.
use std::path::Path;

use fbuild_core::FbuildError;
use fbuild_core::path::NormalizedPath;

use crate::PackageBase;

impl PackageBase {
    /// The complete install already on disk (shared tree first, then this
    /// chip's slice), refreshing its LRU recency on a hit.
    pub(crate) fn existing_complete_install(&self) -> Option<NormalizedPath> {
        let found = self
            .installed_dirs()
            .into_iter()
            .find(|path| Self::is_complete_install(path.as_path()))?;
        // An existing-install hit is a use: refresh the DiskCache LRU so
        // background GC doesn't treat this package as the oldest unused
        // entry and delete it mid-build (FastLED/fbuild#1341).
        self.touch_disk_cache();
        Some(found)
    }

    /// Move a sentinel-less entry at `target_path` out of the way so the
    /// staged install can commit there.
    ///
    /// Must only be called while holding the install lock for `target_path`.
    /// The entry is renamed aside first (atomic, so the final path is never
    /// half-deleted) and the quarantined copy is then removed best-effort.
    pub(crate) fn quarantine_incomplete_install(
        &self,
        target_path: &Path,
    ) -> fbuild_core::Result<()> {
        if std::fs::symlink_metadata(target_path).is_err() {
            return Ok(());
        }
        let name = target_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        let aside = target_path.with_file_name(format!("{name}_incomplete"));
        remove_entry(&aside);
        tracing::warn!(
            "{} v{}: {} has no install sentinel; re-installing",
            self.name,
            self.version,
            target_path.display()
        );
        fbuild_core::platform::fs::rename_path(target_path, &aside).map_err(|e| {
            FbuildError::PackageError(format!(
                "failed to move incomplete install {} aside: {e}",
                target_path.display()
            ))
        })?;
        remove_entry(&aside);
        Ok(())
    }
}

/// Best-effort removal of a file or directory tree.
fn remove_entry(path: &Path) {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return;
    };
    let result = if meta.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    if let Err(e) = result {
        tracing::warn!("failed to remove {}: {e}", path.display());
    }
}
