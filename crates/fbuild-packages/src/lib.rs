//! Package management: toolchain resolution, library downloads, caching.
//!
//! Handles URL-based package management with:
//! - Cache with URL-hashed directory isolation
//! - HTTP download with SHA256 checksum verification
//! - Archive extraction (tar.gz, tar.bz2, tar.xz, zip)
//! - Package, Toolchain, and Framework traits

pub mod cache;
pub mod downloader;
pub mod extractor;
pub mod library;
pub mod toolchain;

pub use cache::Cache;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
}

/// Which cache subdirectory to use.
#[derive(Debug, Clone, Copy)]
pub enum CacheSubdir {
    Toolchains,
    Platforms,
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
        Self {
            name: name.to_string(),
            version: version.to_string(),
            url: url.to_string(),
            cache_key: cache_key.to_string(),
            checksum: checksum.map(|s| s.to_string()),
            cache: Cache::new(project_dir),
            cache_subdir,
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
    pub fn is_cached(&self) -> bool {
        let path = self.install_path();
        path.exists() && path.is_dir()
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
