//! Garbage collection for the two-phase disk cache.
//!
//! Eviction order (cheap to expensive):
//! 1. Installed directories (LRU-first, skip pinned/leased)
//! 2. Archive files (LRU-first, skip leased)
//!
//! GC stops at LOW_WATERMARK to avoid over-evicting.

use super::budget::CacheBudget;
use super::index::CacheIndex;
use std::path::{Path, PathBuf};

/// Report of a GC run.
#[derive(Debug, Clone, Default)]
pub struct GcReport {
    pub installed_evicted: u64,
    pub installed_bytes_freed: u64,
    pub archives_evicted: u64,
    pub archive_bytes_freed: u64,
    pub leases_reaped: usize,
    pub orphan_files_removed: usize,
    pub orphan_rows_cleaned: usize,
}

impl GcReport {
    pub fn total_bytes_freed(&self) -> u64 {
        self.installed_bytes_freed + self.archive_bytes_freed
    }
}

impl std::fmt::Display for GcReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "GC: freed {} installed ({} bytes), {} archives ({} bytes), \
             reaped {} leases, {} orphan files, {} orphan rows",
            self.installed_evicted,
            self.installed_bytes_freed,
            self.archives_evicted,
            self.archive_bytes_freed,
            self.leases_reaped,
            self.orphan_files_removed,
            self.orphan_rows_cleaned,
        )
    }
}

/// Run a full GC pass against the index and on-disk state.
pub fn run_gc(index: &CacheIndex, budget: &CacheBudget) -> rusqlite::Result<GcReport> {
    let leases_reaped = index.reap_dead_leases()?;
    let mut report = GcReport {
        leases_reaped,
        ..Default::default()
    };

    // Step 1: evict installed directories if over budget
    let mut installed_bytes = index.total_installed_bytes()?.max(0) as u64;
    let total_bytes = index.total_archive_bytes()?.max(0) as u64 + installed_bytes;

    if installed_bytes > budget.installed_budget || total_bytes > budget.high_watermark {
        let target = budget.low_watermark.min(budget.installed_budget);
        let entries = index.lru_installed_entries(1000)?;

        for entry in entries {
            if installed_bytes <= target {
                break;
            }
            let bytes = entry.installed_bytes.unwrap_or(0) as u64;

            // Remove the directory from disk
            if let Some(ref path) = entry.installed_path {
                let full_path = index.cache_root().join(path);
                match std::fs::remove_dir_all(&full_path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => {
                        tracing::warn!("GC: failed to remove {}: {}", full_path.display(), e);
                        continue;
                    }
                }
            }

            index.clear_installed(entry.id)?;
            installed_bytes = installed_bytes.saturating_sub(bytes);
            report.installed_evicted += 1;
            report.installed_bytes_freed += bytes;
        }
    }

    // Step 2: evict archives if over per-phase budget OR combined high watermark
    let mut archive_bytes = index.total_archive_bytes()?.max(0) as u64;
    let mut total_bytes = archive_bytes + installed_bytes;

    if archive_bytes > budget.archive_budget || total_bytes > budget.high_watermark {
        let entries = index.lru_archive_entries(1000)?;

        for entry in entries {
            if archive_bytes <= budget.archive_budget && total_bytes <= budget.high_watermark {
                break;
            }
            let bytes = entry.archive_bytes.unwrap_or(0) as u64;

            // Remove the archive file from disk
            if let Some(ref path) = entry.archive_path {
                let full_path = index.cache_root().join(path);
                match remove_path(&full_path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => {
                        tracing::warn!("GC: failed to remove {}: {}", full_path.display(), e);
                        continue;
                    }
                }
            }

            index.clear_archive(entry.id)?;
            archive_bytes = archive_bytes.saturating_sub(bytes);
            total_bytes = total_bytes.saturating_sub(bytes);
            report.archives_evicted += 1;
            report.archive_bytes_freed += bytes;

            // If both archive and installed are gone, delete the row
            if entry.installed_path.is_none() {
                index.delete_entry(entry.id)?;
            }
        }
    }

    Ok(report)
}

/// Reconcile the index against the filesystem.
///
/// - Entries pointing to missing paths → null out the column
/// - Files on disk with no index entry → delete (orphans from crashes)
pub fn reconcile(index: &CacheIndex) -> rusqlite::Result<GcReport> {
    let mut report = GcReport::default();
    let cache_root = index.cache_root().to_path_buf();

    // Phase 1: check all entries, null out paths that don't exist on disk
    let entries = index.all_entries()?;
    for entry in &entries {
        if let Some(ref path) = entry.archive_path {
            let full = cache_root.join(path);
            if !full.exists() {
                index.clear_archive(entry.id)?;
                report.orphan_rows_cleaned += 1;
            }
        }
        if let Some(ref path) = entry.installed_path {
            let full = cache_root.join(path);
            if !full.exists() {
                index.clear_installed(entry.id)?;
                report.orphan_rows_cleaned += 1;
            } else {
                // Check for .install_complete sentinel
                let sentinel = super::paths::install_complete_sentinel(&full);
                if !sentinel.exists() {
                    // Partial install — remove it
                    let _ = std::fs::remove_dir_all(&full);
                    index.clear_installed(entry.id)?;
                    report.orphan_files_removed += 1;
                }
            }
        }
    }

    // Phase 2: walk filesystem, remove orphan files not in the index
    // Collect all known relative paths from the index for membership checks
    let all_entries = index.all_entries()?;
    let mut known_paths = std::collections::HashSet::new();
    for entry in &all_entries {
        if let Some(ref p) = entry.archive_path {
            known_paths.insert(cache_root.join(p));
        }
        if let Some(ref p) = entry.installed_path {
            known_paths.insert(cache_root.join(p));
        }
    }

    for phase_root in &[
        super::paths::archives_root(&cache_root),
        super::paths::installed_root(&cache_root),
    ] {
        if !phase_root.exists() {
            continue;
        }
        remove_partial_dirs(phase_root, &mut report);
        remove_orphan_entries(phase_root, &known_paths, &mut report);
    }

    Ok(report)
}

/// Remove leaf directories (version-level) that are not tracked by the index.
fn remove_orphan_entries(
    root: &Path,
    known_paths: &std::collections::HashSet<PathBuf>,
    report: &mut GcReport,
) {
    // Walk two levels: kind/stem/hash/version
    if let Ok(entries) = walkdir_sync(root) {
        for path in entries {
            if !path.is_dir() {
                continue;
            }
            // Skip .partial dirs (handled separately)
            if path
                .file_name()
                .map(|n| n.to_string_lossy().ends_with(".partial"))
                .unwrap_or(false)
            {
                continue;
            }
            // Only remove leaf dirs (those with no subdirectories)
            let has_subdirs = std::fs::read_dir(&path)
                .map(|rd| rd.filter_map(|e| e.ok()).any(|e| e.path().is_dir()))
                .unwrap_or(true);
            if has_subdirs {
                continue;
            }
            // If this leaf dir isn't known to the index, it's an orphan
            if !known_paths.contains(&path) {
                let _ = std::fs::remove_dir_all(&path);
                report.orphan_files_removed += 1;
            }
        }
    }
}

/// Remove any `.partial` directories (incomplete downloads/installs).
fn remove_partial_dirs(root: &Path, report: &mut GcReport) {
    if let Ok(walker) = walkdir_sync(root) {
        for entry in walker {
            let path = entry;
            if path.is_dir()
                && path
                    .file_name()
                    .map(|n| n.to_string_lossy().ends_with(".partial"))
                    .unwrap_or(false)
            {
                let _ = std::fs::remove_dir_all(&path);
                report.orphan_files_removed += 1;
            }
        }
    }
}

/// Simple recursive directory walker (no external dependency needed here).
/// Uses symlink_metadata to avoid following symlinks and prevent infinite recursion.
fn walkdir_sync(root: &Path) -> std::io::Result<Vec<std::path::PathBuf>> {
    let mut result = Vec::new();
    if root.is_dir() {
        for entry in std::fs::read_dir(root)? {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let path = entry.path();
            // Use symlink_metadata to avoid following symlinks
            let meta = match std::fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            result.push(path.clone());
            // Only recurse into real directories, not symlinks
            if meta.is_dir() {
                if let Ok(children) = walkdir_sync(&path) {
                    result.extend(children);
                }
            }
        }
    }
    Ok(result)
}

/// Remove a file or directory.
fn remove_path(path: &Path) -> std::io::Result<()> {
    if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

#[cfg(test)]
mod tests {
    use super::super::index::CacheIndex;
    use super::super::paths::Kind;
    use super::*;

    fn make_budget(archive: u64, installed: u64, high: u64) -> CacheBudget {
        CacheBudget {
            archive_budget: archive,
            installed_budget: installed,
            high_watermark: high,
            low_watermark: (high as f64 * 0.80) as u64,
        }
    }

    #[test]
    fn test_gc_no_eviction_under_budget() {
        let idx = CacheIndex::open_in_memory().unwrap();
        idx.record_install(Kind::Packages, "https://a.com/a", "1.0", "p", 100)
            .unwrap();

        let budget = make_budget(1_000_000, 1_000_000, 2_000_000);
        let report = run_gc(&idx, &budget).unwrap();
        assert_eq!(report.installed_evicted, 0);
        assert_eq!(report.archives_evicted, 0);
    }

    #[test]
    fn test_gc_evicts_installed_first() {
        let tmp = tempfile::TempDir::new().unwrap();
        let idx = CacheIndex::open(tmp.path()).unwrap();

        // installed: 3000 bytes over budget of 2000
        idx.record_install(
            Kind::Packages,
            "https://a.com/a",
            "1.0",
            "installed/a",
            1000,
        )
        .unwrap();
        idx.record_install(
            Kind::Packages,
            "https://b.com/b",
            "1.0",
            "installed/b",
            1000,
        )
        .unwrap();
        idx.record_install(
            Kind::Packages,
            "https://c.com/c",
            "1.0",
            "installed/c",
            1000,
        )
        .unwrap();

        // archive: 500 bytes within budget
        idx.record_archive(
            Kind::Packages,
            "https://d.com/d",
            "1.0",
            "archives/d",
            500,
            "sha",
        )
        .unwrap();

        let budget = make_budget(10000, 2000, 5000);
        let report = run_gc(&idx, &budget).unwrap();

        // Should evict installed to get under budget, archives untouched
        assert!(report.installed_evicted > 0);
        assert_eq!(report.archives_evicted, 0);
    }

    #[test]
    fn test_gc_evicts_archives_when_over_budget() {
        let tmp = tempfile::TempDir::new().unwrap();
        let idx = CacheIndex::open(tmp.path()).unwrap();

        // archives: 3000 bytes over budget of 1000
        idx.record_archive(
            Kind::Packages,
            "https://a.com/a",
            "1.0",
            "archives/a",
            1000,
            "s1",
        )
        .unwrap();
        idx.record_archive(
            Kind::Packages,
            "https://b.com/b",
            "1.0",
            "archives/b",
            1000,
            "s2",
        )
        .unwrap();
        idx.record_archive(
            Kind::Packages,
            "https://c.com/c",
            "1.0",
            "archives/c",
            1000,
            "s3",
        )
        .unwrap();

        let budget = make_budget(1000, 10000, 20000);
        let report = run_gc(&idx, &budget).unwrap();

        assert!(report.archives_evicted > 0);
        assert!(report.archive_bytes_freed > 0);
    }

    #[test]
    fn test_gc_lease_blocks_eviction() {
        let tmp = tempfile::TempDir::new().unwrap();
        let idx = CacheIndex::open(tmp.path()).unwrap();

        // One installed entry, pinned
        let entry = idx
            .record_install(
                Kind::Packages,
                "https://a.com/a",
                "1.0",
                "installed/a",
                5000,
            )
            .unwrap();
        idx.pin(entry.id, std::process::id(), 1).unwrap();

        // Budget is tiny — would normally evict
        let budget = make_budget(100, 100, 200);
        let report = run_gc(&idx, &budget).unwrap();

        // But it's pinned, so no eviction
        assert_eq!(report.installed_evicted, 0);
    }

    #[test]
    fn test_gc_low_watermark_stops_eviction() {
        let tmp = tempfile::TempDir::new().unwrap();
        let idx = CacheIndex::open(tmp.path()).unwrap();

        // 10 entries, 100 bytes each = 1000 total
        for i in 0..10 {
            idx.record_install(
                Kind::Packages,
                &format!("https://x.com/{}", i),
                "1.0",
                &format!("installed/{}", i),
                100,
            )
            .unwrap();
        }

        // installed_budget = 500, high_watermark = 1200, low_watermark = 960
        // Combined = 1000, not > high_watermark (1200), but installed (1000) > installed_budget (500)
        // Should evict down to low_watermark min installed_budget = 500
        let budget = make_budget(10000, 500, 1200);
        let report = run_gc(&idx, &budget).unwrap();

        // Should have evicted some entries
        assert!(report.installed_evicted > 0);
        // But not all of them
        let remaining = idx.total_installed_bytes().unwrap() as u64;
        assert!(remaining <= 500, "remaining {} should be <= 500", remaining);
    }

    #[test]
    fn test_reconcile_removes_partial_dirs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let idx = CacheIndex::open(tmp.path()).unwrap();

        // Create a .partial directory in installed/
        let partial = super::super::paths::installed_root(tmp.path())
            .join("packages")
            .join("test")
            .join("abc")
            .join("1.0.partial");
        std::fs::create_dir_all(&partial).unwrap();
        assert!(partial.exists());

        let report = reconcile(&idx).unwrap();
        assert!(report.orphan_files_removed > 0);
        assert!(!partial.exists());
    }
}
