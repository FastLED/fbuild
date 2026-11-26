//! Daemon-side staging GC for extracted prebuilt framework trees (#1882).
//!
//! [`fbuild_packages::prebuilt::extract`] builds a tree in a
//! `.<sha16>.<attempt>.staging` sibling of its destination and removes it on
//! every ordinary error path. A process killed outright (SIGKILL, the OOM
//! killer, a reboot) cannot run that cleanup, so the directory survives holding
//! up to the decompressed size ceiling — one abandoned attempt per kill.
//!
//! The reaping rules live in `fbuild_packages::prebuilt`; this module is the
//! call site. Decision 4 of `docs/design/artifact-serving-decisions.md` makes
//! the sweep daemon-owned: **on daemon startup and every 6 h, on a blocking
//! task that never touches the request path** — the same shape the package
//! cache's own background GC already has (FastLED/fbuild#808).

use fbuild_core::path::NormalizedPath;
use fbuild_packages::prebuilt::{GcError, MIN_AGE, SweepReport, sweep_staging_tree_async};
use std::path::Path;
use std::time::Duration;

/// Interval between sweeps once the daemon is up (decision 4: "every 6 h").
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// Sweep the prebuilt tree root under `cache_root` once.
///
/// Nothing is logged for a quiet sweep: on a healthy cache every pass finds no
/// candidates, and this runs four times a day on every install.
pub async fn sweep_once(cache_root: &Path) -> Result<SweepReport, GcError> {
    let root = NormalizedPath::new(cache_root);
    let report = sweep_staging_tree_async(root, MIN_AGE).await?;
    if report.removed > 0 || report.kept_marked > 0 {
        tracing::info!(
            "prebuilt staging sweep: removed {} abandoned attempt(s), {} freed, kept {} marked + {} live",
            report.removed,
            format_bytes(report.freed_bytes),
            report.kept_marked,
            report.kept_live,
        );
    }
    Ok(report)
}

/// Spawn the sweep: once at startup, then every [`SWEEP_INTERVAL`].
///
/// The walk runs on a blocking task (`sweep_staging_tree_async`), so a slow or
/// very large cache never parks a tokio worker, and no sweep touches the
/// request path.
pub fn spawn_maintenance() {
    tokio::spawn(async move {
        let cache_root = fbuild_paths::get_cache_root();
        loop {
            if let Err(error) = sweep_once(cache_root.as_path()).await {
                tracing::warn!("prebuilt staging sweep failed: {}", error);
            }
            tokio::time::sleep(SWEEP_INTERVAL).await;
        }
    });
}

/// Human byte count for the log line, matching the cache GC's wording.
fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    match bytes {
        b if b >= GIB => format!("{:.1} GiB", b as f64 / GIB as f64),
        b if b >= MIB => format!("{:.1} MiB", b as f64 / MIB as f64),
        b if b >= KIB => format!("{:.1} KiB", b as f64 / KIB as f64),
        b => format!("{b} B"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create `cache/prebuilt/fastled/3.3.12/esp32` and return it.
    fn prebuilt_tree(cache: &Path) -> std::path::PathBuf {
        let tree = cache
            .join("prebuilt")
            .join("fastled")
            .join("3.3.12")
            .join("esp32");
        std::fs::create_dir_all(&tree).unwrap();
        tree
    }

    fn staging_sibling(parent: &Path, attempt: usize) -> std::path::PathBuf {
        let path = parent.join(format!(".0123456789abcdef.{attempt}.staging"));
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("partial.a"), vec![b'x'; 16]).unwrap();
        path
    }

    #[tokio::test]
    async fn the_walk_reaches_a_staging_sibling_under_the_prebuilt_root() {
        let cache = tempfile::tempdir().unwrap();
        let tree = prebuilt_tree(cache.path());
        let live = staging_sibling(&tree, 0);
        let report = sweep_once(cache.path()).await.unwrap();
        // A fresh attempt is *kept* and counted: the count is what proves the
        // walk reached the leaf, not just the root.
        assert_eq!(report.kept_live, 1);
        assert_eq!(report.removed, 0);
        assert!(live.is_dir());
    }

    #[tokio::test]
    async fn a_staging_directory_outside_the_prebuilt_root_is_not_swept() {
        let cache = tempfile::tempdir().unwrap();
        let foreign = cache.path().join("archives").join("tree");
        std::fs::create_dir_all(&foreign).unwrap();
        let kept = staging_sibling(&foreign, 3);
        let report = sweep_once(cache.path()).await.unwrap();
        assert_eq!(report, SweepReport::default());
        assert!(kept.is_dir());
    }

    #[tokio::test]
    async fn a_cache_without_a_prebuilt_root_is_a_quiet_sweep() {
        let cache = tempfile::tempdir().unwrap();
        let report = sweep_once(cache.path()).await.unwrap();
        assert_eq!(report, SweepReport::default());
    }

    #[test]
    fn byte_counts_are_readable() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(2048), "2.0 KiB");
        assert_eq!(format_bytes(5 * 1024 * 1024), "5.0 MiB");
    }
}
