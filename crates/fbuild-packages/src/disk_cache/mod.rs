//! Two-phase disk cache with LRU garbage collection and crash-safe SQLite index.
//!
//! Separates downloaded archives from installed (extracted) content.
//! GC evicts cheap-to-rehydrate installed directories before expensive archives.
//!
//! # Usage
//!
//! ```ignore
//! let cache = DiskCache::open()?;
//! if let Some(entry) = cache.lookup(Kind::Toolchains, url, version)? {
//!     let _lease = cache.lease(&entry)?;  // pin during build
//!     cache.touch(&entry)?;
//!     // use entry.installed_path ...
//! }
//! ```

pub mod budget;
pub mod gc;
pub mod index;
pub mod lease;
pub mod paths;

pub use budget::CacheBudget;
pub use gc::GcReport;
pub use index::{CacheEntry, CacheIndex};
pub use lease::Lease;
pub use paths::Kind;

use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The public facade for the two-phase disk cache.
pub struct DiskCache {
    index: Arc<CacheIndex>,
    cache_root: PathBuf,
    budget: CacheBudget,
}

impl DiskCache {
    /// Open the disk cache at the standard location.
    pub fn open() -> rusqlite::Result<Self> {
        let cache_root = fbuild_paths::get_cache_root();
        Self::open_at(&cache_root)
    }

    /// Open the disk cache at a specific root (for testing).
    pub fn open_at(cache_root: &Path) -> rusqlite::Result<Self> {
        let index = CacheIndex::open(cache_root)?;
        let budget = CacheBudget::compute(cache_root);

        // Ensure phase directories exist — propagate failures so callers
        // don't silently operate on an unusable cache layout.
        let map_io = |e: std::io::Error| {
            rusqlite::Error::InvalidPath(PathBuf::from(format!(
                "failed to create cache phase dir: {}",
                e
            )))
        };
        std::fs::create_dir_all(paths::archives_root(cache_root)).map_err(&map_io)?;
        std::fs::create_dir_all(paths::installed_root(cache_root)).map_err(map_io)?;

        Ok(Self {
            index: Arc::new(index),
            cache_root: cache_root.to_path_buf(),
            budget,
        })
    }

    /// Look up a cache entry.
    pub fn lookup(
        &self,
        kind: Kind,
        url: &str,
        version: &str,
    ) -> rusqlite::Result<Option<CacheEntry>> {
        self.index.lookup(kind, url, version)
    }

    /// Record that an archive was downloaded.
    pub fn record_archive(
        &self,
        kind: Kind,
        url: &str,
        version: &str,
        archive_path: &str,
        archive_bytes: i64,
        archive_sha256: &str,
    ) -> rusqlite::Result<CacheEntry> {
        self.index.record_archive(
            kind,
            url,
            version,
            archive_path,
            archive_bytes,
            archive_sha256,
        )
    }

    /// Record that an entry was installed (extracted from archive).
    pub fn record_install(
        &self,
        kind: Kind,
        url: &str,
        version: &str,
        installed_path: &str,
        installed_bytes: i64,
    ) -> rusqlite::Result<CacheEntry> {
        self.index
            .record_install(kind, url, version, installed_path, installed_bytes)
    }

    /// Acquire a lease for the given entry, preventing GC eviction.
    pub fn lease(&self, entry: &CacheEntry) -> rusqlite::Result<Lease> {
        Lease::acquire(Arc::clone(&self.index), entry.id)
    }

    /// Bump LRU timestamp for a cache hit.
    pub fn touch(&self, entry: &CacheEntry) -> rusqlite::Result<()> {
        self.index.touch(entry.id)
    }

    /// Run a full GC pass.
    /// Recomputes budgets from current disk space so long-lived processes
    /// don't enforce stale watermarks.
    pub fn run_gc(&self) -> rusqlite::Result<GcReport> {
        let budget = CacheBudget::compute(&self.cache_root);
        gc::run_gc(&self.index, &budget)
    }

    /// Reconcile index against filesystem (run on daemon startup).
    pub fn reconcile(&self) -> rusqlite::Result<GcReport> {
        gc::reconcile(&self.index)
    }

    /// Get cache statistics.
    pub fn stats(&self) -> rusqlite::Result<CacheStats> {
        Ok(CacheStats {
            archive_bytes: self.index.total_archive_bytes()? as u64,
            installed_bytes: self.index.total_installed_bytes()? as u64,
            entry_count: self.index.entry_count()?,
            budget: self.budget,
        })
    }

    /// Get the cache root path.
    pub fn cache_root(&self) -> &Path {
        &self.cache_root
    }

    /// Get the computed budget.
    pub fn budget(&self) -> &CacheBudget {
        &self.budget
    }

    // --- Path helpers for callers ---

    /// Get the archive entry directory for a given kind/url/version.
    pub fn archive_dir(&self, kind: Kind, url: &str, version: &str) -> PathBuf {
        paths::archive_entry_dir(&self.cache_root, kind, url, version)
    }

    /// Get the staging directory for an in-progress archive download.
    pub fn archive_staging_dir(&self, kind: Kind, url: &str, version: &str) -> PathBuf {
        paths::archive_staging_dir(&self.cache_root, kind, url, version)
    }

    /// Get the installed entry directory.
    pub fn installed_dir(&self, kind: Kind, url: &str, version: &str) -> PathBuf {
        paths::installed_entry_dir(&self.cache_root, kind, url, version)
    }

    /// Get the staging directory for an in-progress installation.
    pub fn install_staging_dir(&self, kind: Kind, url: &str, version: &str) -> PathBuf {
        paths::install_staging_dir(&self.cache_root, kind, url, version)
    }
}

/// Cache statistics for `fbuild status`.
#[derive(Debug, Clone)]
pub struct CacheStats {
    pub archive_bytes: u64,
    pub installed_bytes: u64,
    pub entry_count: i64,
    pub budget: CacheBudget,
}

impl CacheStats {
    pub fn total_bytes(&self) -> u64 {
        self.archive_bytes + self.installed_bytes
    }
}

impl std::fmt::Display for CacheStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Cache: {} entries, {} installed + {} archives = {} total (budget: {} high, {} low)",
            self.entry_count,
            format_bytes(self.installed_bytes),
            format_bytes(self.archive_bytes),
            format_bytes(self.total_bytes()),
            format_bytes(self.budget.high_watermark),
            format_bytes(self.budget.low_watermark),
        )
    }
}

fn format_bytes(bytes: u64) -> String {
    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;
    const KIB: u64 = 1024;

    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{} B", bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_disk_cache_open_and_stats() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cache = DiskCache::open_at(tmp.path()).unwrap();
        let stats = cache.stats().unwrap();
        assert_eq!(stats.entry_count, 0);
        assert_eq!(stats.archive_bytes, 0);
        assert_eq!(stats.installed_bytes, 0);
    }

    #[test]
    fn test_disk_cache_full_workflow() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cache = DiskCache::open_at(tmp.path()).unwrap();
        let url = "https://example.com/tool.tar.gz";

        // Record archive
        let entry = cache
            .record_archive(
                Kind::Toolchains,
                url,
                "7.3.0",
                "archives/tool.tar.gz",
                5000,
                "abc123",
            )
            .unwrap();
        assert!(entry.archive_path.is_some());

        // Record install
        let entry = cache
            .record_install(
                Kind::Toolchains,
                url,
                "7.3.0",
                "installed/tool/7.3.0",
                20000,
            )
            .unwrap();
        assert!(entry.installed_path.is_some());

        // Lookup
        let found = cache.lookup(Kind::Toolchains, url, "7.3.0").unwrap();
        assert!(found.is_some());

        // Touch
        cache.touch(&found.unwrap()).unwrap();

        // Stats
        let stats = cache.stats().unwrap();
        assert_eq!(stats.entry_count, 1);
        assert_eq!(stats.archive_bytes, 5000);
        assert_eq!(stats.installed_bytes, 20000);
    }

    #[test]
    fn test_disk_cache_lease_workflow() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cache = DiskCache::open_at(tmp.path()).unwrap();

        let entry = cache
            .record_install(
                Kind::Packages,
                "https://example.com/a",
                "1.0",
                "path_a",
                1000,
            )
            .unwrap();

        // Acquire lease
        let _lease = cache.lease(&entry).unwrap();

        // Entry should be pinned
        let entry = cache
            .lookup(Kind::Packages, "https://example.com/a", "1.0")
            .unwrap()
            .unwrap();
        assert_eq!(entry.pinned, 1);
    }

    #[test]
    fn test_disk_cache_path_helpers() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cache = DiskCache::open_at(tmp.path()).unwrap();
        let url = "https://example.com/pkg.tar.gz";

        let archive_dir = cache.archive_dir(Kind::Packages, url, "1.0");
        assert!(archive_dir.to_string_lossy().contains("archives"));
        assert!(archive_dir.to_string_lossy().contains("packages"));

        let installed_dir = cache.installed_dir(Kind::Packages, url, "1.0");
        assert!(installed_dir.to_string_lossy().contains("installed"));
    }

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1024), "1.0 KiB");
        assert_eq!(format_bytes(1024 * 1024), "1.0 MiB");
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.0 GiB");
        assert_eq!(format_bytes(15 * 1024 * 1024 * 1024), "15.0 GiB");
    }

    #[test]
    fn test_cache_stats_display() {
        let stats = CacheStats {
            archive_bytes: 1024 * 1024 * 100,
            installed_bytes: 1024 * 1024 * 500,
            entry_count: 42,
            budget: CacheBudget::compute_with_disk_size(500 * 1024 * 1024 * 1024),
        };
        let display = format!("{}", stats);
        assert!(display.contains("42 entries"));
    }

    #[test]
    fn test_reconcile_on_open() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cache = DiskCache::open_at(tmp.path()).unwrap();
        // Reconcile should succeed on empty cache
        let report = cache.reconcile().unwrap();
        assert_eq!(report.orphan_files_removed, 0);
    }
}
