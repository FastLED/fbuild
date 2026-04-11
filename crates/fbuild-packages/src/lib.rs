//! Package management: toolchain resolution, library downloads, caching.
//!
//! Handles URL-based package management with:
//! - Cache with URL-hashed directory isolation
//! - HTTP download with SHA256 checksum verification
//! - Archive extraction (tar.gz, tar.bz2, tar.xz, zip)
//! - Package, Toolchain, and Framework traits

pub mod cache;
pub mod disk_cache;
pub mod downloader;
pub mod extractor;
pub mod library;
pub mod toolchain;

pub use cache::Cache;
pub use disk_cache::DiskCache;

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};

/// Recursively compute the total size of a directory in bytes.
fn dir_size(path: &Path) -> u64 {
    std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| {
            let meta = e.metadata().unwrap_or_else(|_| {
                // Fallback: zero-size if metadata fails
                std::fs::metadata(e.path()).unwrap_or_else(|_| unreachable!())
            });
            if meta.is_dir() {
                dir_size(&e.path())
            } else {
                meta.len()
            }
        })
        .sum()
}

pub(crate) fn block_on_package_future<F, T>(future: F) -> fbuild_core::Result<T>
where
    F: Future<Output = fbuild_core::Result<T>>,
{
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        tokio::task::block_in_place(|| handle.block_on(future))
    } else {
        let rt = tokio::runtime::Runtime::new().map_err(|e| {
            fbuild_core::FbuildError::PackageError(format!("failed to create tokio runtime: {}", e))
        })?;
        rt.block_on(future)
    }
}

/// Base trait for all installable packages.
pub trait Package: Send + Sync {
    /// Ensure the package is installed, downloading if necessary.
    fn ensure_installed(&self) -> fbuild_core::Result<PathBuf>;

    /// Check if the package is already installed.
    fn is_installed(&self) -> bool;

    /// Get package metadata.
    fn get_info(&self) -> PackageInfo;
}

/// Base trait for toolchain packages (GCC, etc.).
pub trait Toolchain: Package {
    fn get_gcc_path(&self) -> PathBuf;
    fn get_gxx_path(&self) -> PathBuf;
    fn get_ar_path(&self) -> PathBuf;
    fn get_objcopy_path(&self) -> PathBuf;
    fn get_size_path(&self) -> PathBuf;
    fn get_bin_dir(&self) -> PathBuf;

    /// Path to the LTO-aware archiver (`{prefix}-gcc-ar`).
    ///
    /// Required for LTO-enabled builds: plain `ar` does not insert the LTO
    /// linker-plugin index, which can cause the linker to silently drop
    /// symbols on toolchains where the plugin path isn't auto-discovered.
    /// See ISSUES.md Issue 8 for the full rationale.
    ///
    /// Default implementation derives the path by replacing the `ar`
    /// basename suffix with `gcc-ar`. If the derived binary doesn't exist
    /// on disk, falls back to `get_ar_path()`.
    fn get_gcc_ar_path(&self) -> PathBuf {
        let ar = self.get_ar_path();
        let parent = ar.parent().unwrap_or(Path::new(""));
        let file_name = ar.file_name().and_then(|n| n.to_str()).unwrap_or("ar");
        // Strip platform extension if present (e.g. `.exe`).
        let (stem, ext) = match file_name.rsplit_once('.') {
            Some((s, e)) => (s, format!(".{}", e)),
            None => (file_name, String::new()),
        };
        // Replace trailing `ar` (or `-ar`) with `gcc-ar` (or `-gcc-ar`).
        let gcc_ar_stem = if let Some(prefix) = stem.strip_suffix("-ar") {
            format!("{}-gcc-ar", prefix)
        } else if let Some(prefix) = stem.strip_suffix("ar") {
            format!("{}gcc-ar", prefix)
        } else {
            return ar;
        };
        let candidate = parent.join(format!("{}{}", gcc_ar_stem, ext));
        if candidate.exists() {
            candidate
        } else {
            ar
        }
    }

    /// Get all tool paths as a map.
    fn get_all_tools(&self) -> HashMap<String, PathBuf> {
        let mut tools = HashMap::new();
        tools.insert("gcc".to_string(), self.get_gcc_path());
        tools.insert("g++".to_string(), self.get_gxx_path());
        tools.insert("ar".to_string(), self.get_ar_path());
        tools.insert("objcopy".to_string(), self.get_objcopy_path());
        tools.insert("size".to_string(), self.get_size_path());
        tools
    }

    /// Toolchain include directories (sysroot headers like `xtensa/coreasm.h`).
    ///
    /// GCC cross-compilers may fail to resolve their own sysroot when
    /// relocated. This returns the toolchain's `<root>/include/` directory
    /// so callers can add it explicitly with `-I`.
    fn get_include_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        if let Some(root) = self.get_bin_dir().parent() {
            let inc = root.join("include");
            if inc.is_dir() {
                dirs.push(inc);
            }
        }
        dirs
    }
}

/// Base trait for framework packages (Arduino core, ESP-IDF, etc.).
pub trait Framework: Package {
    fn get_cores_dir(&self) -> PathBuf;
    fn get_variants_dir(&self) -> PathBuf;
    fn get_libraries_dir(&self) -> PathBuf;
}

/// Package metadata.
#[derive(Debug, Clone)]
pub struct PackageInfo {
    pub name: String,
    pub version: String,
    pub url: String,
    pub install_path: PathBuf,
}

/// Shared base for package implementations.
/// Handles download, extraction, cache lookup, and staged installation.
pub struct PackageBase {
    pub name: String,
    pub version: String,
    /// Full download URL.
    pub url: String,
    /// URL used for cache directory hashing. May differ from download URL.
    /// Python fbuild hashes the base URL for toolchains (e.g. `https://downloads.arduino.cc/tools`)
    /// but the full URL for frameworks (e.g. the GitHub archive URL).
    pub cache_key: String,
    pub checksum: Option<String>,
    pub cache: Cache,
    /// Cache subdirectory: "toolchains" or "platforms".
    pub cache_subdir: CacheSubdir,
    /// Optional DiskCache for LRU tracking. Best-effort: `None` if SQLite open fails.
    disk_cache: Option<DiskCache>,
}

/// Which cache subdirectory to use.
#[derive(Debug, Clone, Copy)]
pub enum CacheSubdir {
    Toolchains,
    Platforms,
}

impl From<CacheSubdir> for disk_cache::Kind {
    fn from(subdir: CacheSubdir) -> Self {
        match subdir {
            CacheSubdir::Toolchains => disk_cache::Kind::Toolchains,
            CacheSubdir::Platforms => disk_cache::Kind::Platforms,
        }
    }
}

impl PackageBase {
    pub fn new(
        name: &str,
        version: &str,
        url: &str,
        cache_key: &str,
        checksum: Option<&str>,
        cache_subdir: CacheSubdir,
        project_dir: &Path,
    ) -> Self {
        let cache = Cache::new(project_dir);
        let disk_cache = DiskCache::open().ok();
        Self {
            name: name.to_string(),
            version: version.to_string(),
            url: url.to_string(),
            cache_key: cache_key.to_string(),
            checksum: checksum.map(|s| s.to_string()),
            cache,
            cache_subdir,
            disk_cache,
        }
    }

    /// Create with an explicit cache root (for testing without env vars).
    #[allow(clippy::too_many_arguments)]
    pub fn with_cache_root(
        name: &str,
        version: &str,
        url: &str,
        cache_key: &str,
        checksum: Option<&str>,
        cache_subdir: CacheSubdir,
        project_dir: &Path,
        cache_root: &Path,
    ) -> Self {
        let disk_cache = DiskCache::open_at(cache_root).ok();
        Self {
            name: name.to_string(),
            version: version.to_string(),
            url: url.to_string(),
            cache_key: cache_key.to_string(),
            checksum: checksum.map(|s| s.to_string()),
            cache: Cache::with_cache_root(project_dir, cache_root),
            cache_subdir,
            disk_cache,
        }
    }

    /// Get the install path in the cache.
    pub fn install_path(&self) -> PathBuf {
        match self.cache_subdir {
            CacheSubdir::Toolchains => self
                .cache
                .get_toolchain_path(&self.cache_key, &self.version),
            CacheSubdir::Platforms => self.cache.get_platform_path(&self.cache_key, &self.version),
        }
    }

    /// Check if already installed in cache.
    /// On a cache hit, bumps the LRU timestamp in the DiskCache index.
    pub fn is_cached(&self) -> bool {
        let path = self.install_path();
        let cached = path.exists() && path.is_dir();
        if cached {
            self.touch_disk_cache();
        }
        cached
    }

    /// Best-effort LRU touch in the DiskCache index.
    fn touch_disk_cache(&self) {
        if let Some(ref dc) = self.disk_cache {
            let kind = self.cache_subdir.into();
            if let Ok(Some(entry)) = dc.lookup(kind, &self.cache_key, &self.version) {
                let _ = dc.touch(&entry);
            }
        }
    }

    /// Best-effort: record a completed install in the DiskCache index.
    fn record_install_in_disk_cache(&self, install_path: &Path) {
        if let Some(ref dc) = self.disk_cache {
            let kind = self.cache_subdir.into();
            let installed_bytes = dir_size(install_path) as i64;
            let rel_path = install_path
                .strip_prefix(dc.cache_root())
                .unwrap_or(install_path);
            let _ = dc.record_install(
                kind,
                &self.cache_key,
                &self.version,
                &rel_path.to_string_lossy(),
                installed_bytes,
            );
        }
    }

    /// Download and install with staged directory pattern.
    ///
    /// 1. Download to temp file
    /// 2. Verify checksum
    /// 3. Extract to staging dir (.tmp suffix)
    /// 4. Validate (caller provides validation fn)
    /// 5. Rename staging to final path (atomic commit)
    pub async fn staged_install<F>(&self, validate: F) -> fbuild_core::Result<PathBuf>
    where
        F: FnOnce(&Path) -> fbuild_core::Result<()>,
    {
        let install_path = self.install_path();

        if install_path.exists() {
            return Ok(install_path);
        }

        // Ensure parent directory exists
        if let Some(parent) = install_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                fbuild_core::FbuildError::PackageError(format!("failed to create cache dir: {}", e))
            })?;
        }

        // Append _staging to dir name (can't use with_extension — version has dots)
        let staging_path = install_path.with_file_name(format!(
            "{}_staging",
            install_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        ));

        // Clean up stale staging directory
        if staging_path.exists() {
            let _ = std::fs::remove_dir_all(&staging_path);
        }

        std::fs::create_dir_all(&staging_path).map_err(|e| {
            fbuild_core::FbuildError::PackageError(format!("failed to create staging dir: {}", e))
        })?;

        // Download
        tracing::info!("downloading {} v{}", self.name, self.version);
        let archive_path = downloader::download_file(&self.url, &staging_path).await?;

        // Verify checksum
        if let Some(ref expected) = self.checksum {
            downloader::verify_checksum(&archive_path, expected)?;
        }

        // Extract
        tracing::info!("extracting {} v{}", self.name, self.version);
        extractor::extract(&archive_path, &staging_path)?;

        // Remove the archive after extraction
        let _ = std::fs::remove_file(&archive_path);

        // Validate
        validate(&staging_path)?;

        // Atomic commit: rename staging → final
        std::fs::rename(&staging_path, &install_path).map_err(|e| {
            fbuild_core::FbuildError::PackageError(format!("failed to commit installation: {}", e))
        })?;

        // Write sentinel so GC reconciliation recognizes this as a complete install
        let sentinel = disk_cache::paths::install_complete_sentinel(&install_path);
        std::fs::write(&sentinel, b"").map_err(|e| {
            fbuild_core::FbuildError::PackageError(format!(
                "failed to write install sentinel: {}",
                e
            ))
        })?;

        // Best-effort: record in DiskCache index for LRU tracking
        self.record_install_in_disk_cache(&install_path);

        tracing::info!("installed {} v{}", self.name, self.version);
        Ok(install_path)
    }

    pub fn get_info(&self) -> PackageInfo {
        PackageInfo {
            name: self.name.clone(),
            version: self.version.clone(),
            url: self.url.clone(),
            install_path: self.install_path(),
        }
    }
}

#[cfg(test)]
mod toolchain_gcc_ar_tests {
    use super::*;

    /// Test toolchain that lets the test set the `ar_path`.
    struct TestToolchain {
        ar_path: PathBuf,
    }

    impl Package for TestToolchain {
        fn ensure_installed(&self) -> fbuild_core::Result<PathBuf> {
            Ok(PathBuf::new())
        }
        fn is_installed(&self) -> bool {
            true
        }
        fn get_info(&self) -> PackageInfo {
            PackageInfo {
                name: "test".to_string(),
                version: "0.0".to_string(),
                url: String::new(),
                install_path: PathBuf::new(),
            }
        }
    }

    impl Toolchain for TestToolchain {
        fn get_gcc_path(&self) -> PathBuf {
            PathBuf::new()
        }
        fn get_gxx_path(&self) -> PathBuf {
            PathBuf::new()
        }
        fn get_ar_path(&self) -> PathBuf {
            self.ar_path.clone()
        }
        fn get_objcopy_path(&self) -> PathBuf {
            PathBuf::new()
        }
        fn get_size_path(&self) -> PathBuf {
            PathBuf::new()
        }
        fn get_bin_dir(&self) -> PathBuf {
            self.ar_path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default()
        }
    }

    #[test]
    fn falls_back_to_ar_when_gcc_ar_does_not_exist() {
        // /__bogus__/avr-gcc-ar does not exist on disk → fall back to ar.
        let tc = TestToolchain {
            ar_path: PathBuf::from("/__bogus__/avr-ar"),
        };
        assert_eq!(tc.get_gcc_ar_path(), tc.get_ar_path());
    }

    #[test]
    fn returns_gcc_ar_when_present_on_disk_unix_style() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bin = tmp.path();
        let ar = bin.join("xtensa-esp-elf-ar");
        let gcc_ar = bin.join("xtensa-esp-elf-gcc-ar");
        std::fs::write(&ar, b"").unwrap();
        std::fs::write(&gcc_ar, b"").unwrap();

        let tc = TestToolchain { ar_path: ar };
        assert_eq!(tc.get_gcc_ar_path(), gcc_ar);
    }

    #[test]
    fn returns_gcc_ar_when_present_on_disk_with_exe_suffix() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bin = tmp.path();
        let ar = bin.join("avr-ar.exe");
        let gcc_ar = bin.join("avr-gcc-ar.exe");
        std::fs::write(&ar, b"").unwrap();
        std::fs::write(&gcc_ar, b"").unwrap();

        let tc = TestToolchain { ar_path: ar };
        assert_eq!(tc.get_gcc_ar_path(), gcc_ar);
    }
}
