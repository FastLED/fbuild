//! Package management: toolchain resolution, library downloads, caching.
//!
//! Handles URL-based package management with:
//! - Cache with URL-hashed directory isolation
//! - HTTP download with SHA256 checksum verification
//! - Archive extraction (tar.gz, tar.bz2, tar.xz, zip)
//! - Package, Toolchain, and Framework traits

pub mod cache;
pub mod cache_archive;
pub mod disk_cache;
pub mod downloader;
pub mod extractor;
pub mod http;
pub mod library;
pub mod lnk;
pub mod toolchain;

mod install_lock;

pub use cache::Cache;
pub use disk_cache::DiskCache;
pub use lnk::{ExtractMode, LnkFile};

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use async_trait::async_trait;
use fbuild_core::install_status::{self, InstallPhase, InstallRole};

static PACKAGE_TOUCHES: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

/// Recursively compute the total size of a directory in bytes.
///
/// Symlink-safe: uses `symlink_metadata` and skips symlinks to avoid
/// infinite recursion. Tolerates permission errors by treating
/// inaccessible entries as zero-size.
fn dir_size(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let meta = std::fs::symlink_metadata(e.path()).ok()?;
            if meta.is_symlink() {
                None // skip symlinks to avoid cycles
            } else if meta.is_dir() {
                Some(dir_size(&e.path()))
            } else {
                Some(meta.len())
            }
        })
        .sum()
}

/// Base trait for all installable packages.
///
/// FastLED/fbuild#813: `ensure_installed` is async so it composes with the
/// daemon's tokio reactor and tokio-console sees every package install as
/// part of the task graph. Use `#[async_trait]` on impls.
#[async_trait]
pub trait Package: Send + Sync {
    /// Ensure the package is installed, downloading if necessary.
    async fn ensure_installed(&self) -> fbuild_core::Result<PathBuf>;

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

    /// Apply a consumer-provided override (e.g. parsed from `platform_packages`
    /// in `platformio.ini`).
    ///
    /// Replaces `url`, `cache_key` (← override URL), `version`, and `checksum`.
    /// Preserves `name` and `cache_subdir`. The cache-key swap is what gives the
    /// override its own subdir under `~/.fbuild/<env>/cache/<kind>/<stem>/<hash>/<version>/`,
    /// so two different commit URLs hash to two different directories and a
    /// bisection workflow doesn't fight the default cache.
    ///
    /// `checksum: None` skips sha256 verification — consumer-trusted, which is
    /// the right policy for `platform_packages` overrides (#681, sibling of #663).
    ///
    /// Emits a single INFO log so the override is visible in build scrollback
    /// across every framework package, without each orchestrator having to
    /// remember to log it themselves.
    pub fn with_override(mut self, ovr: fbuild_config::PackageOverride) -> Self {
        tracing::info!("{} OVERRIDE: {} (was {})", self.name, ovr.url, self.url);
        self.url = ovr.url.clone();
        self.cache_key = ovr.url;
        self.version = ovr.version;
        self.checksum = ovr.checksum;
        self
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
            let touch_key =
                package_touch_key(kind, &self.cache_key, &self.version, &self.install_path());
            if !mark_package_touch_needed(touch_key) {
                return;
            }
            if let Ok(Some(entry)) = dc.lookup(kind, &self.cache_key, &self.version) {
                let _ = dc.touch(&entry);
            }
        }
    }

    /// Best-effort: record a completed install in the DiskCache index.
    /// Only records if `install_path` is under the DiskCache root — legacy
    /// Cache paths are skipped to avoid indexing absolute legacy paths.
    fn record_install_in_disk_cache(&self, install_path: &Path) {
        if let Some(ref dc) = self.disk_cache {
            let rel_path = match install_path.strip_prefix(dc.cache_root()) {
                Ok(rel) => rel,
                Err(_) => return, // legacy path outside DiskCache root
            };
            let kind = self.cache_subdir.into();
            let installed_bytes = dir_size(install_path) as i64;
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
        F: FnOnce(&Path) -> fbuild_core::Result<()> + Send,
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
        let _install_lock =
            install_lock::acquire_for_install(&install_path, &self.name, &self.version).await?;
        if install_path.exists() {
            return Ok(install_path);
        }

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
        install_status::publish_install_status(install_status::status(
            &self.name,
            Some(&self.version),
            InstallPhase::Downloading,
            InstallRole::Installer,
            format!("downloading {} {}", self.name, self.version),
            None::<String>,
        ));
        tracing::info!("downloading {} v{}", self.name, self.version);
        let archive_path = downloader::download_file(&self.url, &staging_path).await?;

        // Verify checksum
        if let Some(ref expected) = self.checksum {
            install_status::publish_install_status(install_status::status(
                &self.name,
                Some(&self.version),
                InstallPhase::Verifying,
                InstallRole::Installer,
                format!("verifying {} {}", self.name, self.version),
                None::<String>,
            ));
            downloader::verify_checksum(&archive_path, expected)?;
        }

        // Extract
        install_status::publish_install_status(install_status::status(
            &self.name,
            Some(&self.version),
            InstallPhase::Extracting,
            InstallRole::Installer,
            format!("extracting {} {}", self.name, self.version),
            None::<String>,
        ));
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

        // Write sentinel so GC reconciliation recognizes this as a complete install.
        // Best-effort: the install succeeded (rename was atomic), so a sentinel
        // failure should not cause the caller to see an error.
        let sentinel = disk_cache::paths::install_complete_sentinel(&install_path);
        if let Err(e) = std::fs::write(&sentinel, b"") {
            tracing::warn!("failed to write install sentinel: {}", e);
        }

        // Best-effort: record in DiskCache index for LRU tracking
        self.record_install_in_disk_cache(&install_path);

        install_status::publish_install_status(install_status::status(
            &self.name,
            Some(&self.version),
            InstallPhase::Installed,
            InstallRole::Installer,
            format!("installed {} {}", self.name, self.version),
            None::<String>,
        ));
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

fn package_touch_key(
    kind: disk_cache::Kind,
    cache_key: &str,
    version: &str,
    install_path: &Path,
) -> String {
    format!(
        "{}|{}|{}|{}",
        kind.as_str(),
        cache_key,
        version,
        install_path.display()
    )
}

fn mark_package_touch_needed(key: String) -> bool {
    let touched = PACKAGE_TOUCHES.get_or_init(|| Mutex::new(HashSet::new()));
    let mut touched = touched.lock().unwrap_or_else(|e| e.into_inner());
    touched.insert(key)
}

#[cfg(test)]
fn clear_package_touch_cache_for_tests() {
    if let Some(touched) = PACKAGE_TOUCHES.get() {
        touched.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

#[cfg(test)]
mod toolchain_gcc_ar_tests {
    use super::*;

    async fn serve_once(body: Vec<u8>) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/package.zip", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept download");
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf).await;
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(header.as_bytes()).await.unwrap();
            stream.write_all(&body).await.unwrap();
            let _ = stream.shutdown().await;
        });
        url
    }

    fn zip_bytes(entry_name: &str, content: &[u8]) -> Vec<u8> {
        use std::io::{Cursor, Write};
        use zip::write::SimpleFileOptions;

        let cursor = Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(cursor);
        zip.start_file(entry_name, SimpleFileOptions::default())
            .expect("start zip entry");
        zip.write_all(content).expect("write zip entry");
        zip.finish().expect("finish zip").into_inner()
    }

    /// Test toolchain that lets the test set the `ar_path`.
    struct TestToolchain {
        ar_path: PathBuf,
    }

    #[async_trait]
    impl Package for TestToolchain {
        async fn ensure_installed(&self) -> fbuild_core::Result<PathBuf> {
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

    #[test]
    fn package_cache_hit_touch_is_throttled_per_process() {
        clear_package_touch_cache_for_tests();

        let tmp = tempfile::TempDir::new().unwrap();
        let cache_root = tmp.path().join("cache");
        let cache_key = "https://example.com/tool.tar.gz";
        let base = PackageBase::with_cache_root(
            "tool",
            "1.0",
            cache_key,
            cache_key,
            None,
            CacheSubdir::Toolchains,
            tmp.path(),
            &cache_root,
        );
        let install_path = base.install_path();
        std::fs::create_dir_all(&install_path).unwrap();

        let disk_cache = DiskCache::open_at(&cache_root).unwrap();
        let rel_path = install_path.strip_prefix(disk_cache.cache_root()).unwrap();
        disk_cache
            .record_install(
                disk_cache::Kind::Toolchains,
                cache_key,
                "1.0",
                &rel_path.to_string_lossy(),
                1,
            )
            .unwrap();

        assert!(base.is_cached());
        assert!(base.is_cached());

        let entry = disk_cache
            .lookup(disk_cache::Kind::Toolchains, cache_key, "1.0")
            .unwrap()
            .unwrap();
        assert_eq!(
            entry.use_count, 1,
            "repeated cache hits for the same package should not repeatedly write the index"
        );
    }

    #[tokio::test]
    async fn staged_install_removes_stale_legacy_staging_dir_before_retry() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cache_root = tmp.path().join("cache");
        let cache_key = "stale-staging-tool";
        let url = serve_once(zip_bytes("fresh.txt", b"fresh install")).await;
        let base = PackageBase::with_cache_root(
            "tool",
            "1.0",
            &url,
            cache_key,
            None,
            CacheSubdir::Toolchains,
            tmp.path(),
            &cache_root,
        );

        let install_path = base.install_path();
        let staging_path = install_path.with_file_name(format!(
            "{}_staging",
            install_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        ));
        std::fs::create_dir_all(&staging_path).unwrap();
        std::fs::write(staging_path.join("stale.txt"), b"stale interrupted install").unwrap();

        let installed = base
            .staged_install(|staging| {
                assert!(
                    !staging.join("stale.txt").exists(),
                    "stale interrupted install content must be removed before retry"
                );
                assert!(
                    staging.join("fresh.txt").exists(),
                    "fresh archive contents must be extracted into clean staging"
                );
                Ok(())
            })
            .await
            .expect("staged install should succeed");

        assert_eq!(installed, install_path);
        assert!(installed.join("fresh.txt").exists());
        assert!(!installed.join("stale.txt").exists());
        assert!(
            !staging_path.exists(),
            "staging dir is renamed away on commit"
        );
        assert!(disk_cache::paths::install_complete_sentinel(&installed).exists());
    }
}

#[cfg(test)]
mod package_override_tests {
    //! Cache-key uniqueness tests for `PackageBase::with_override`.
    //!
    //! The contract that FastLED/fbuild#681 promises every framework
    //! orchestrator is: when a consumer writes
    //! `platform_packages = framework-x@<URL>#<sha>` in `platformio.ini`,
    //! the resulting cache dir is **distinct** from the default pin's cache
    //! dir AND distinct from any other override at a different commit on the
    //! same upstream URL. Without that, bisection workflows like
    //! FastLED/FastLED#3325 silently reuse the wrong vendored sources.
    //!
    //! These tests pin the contract at the `PackageBase` level so the
    //! invariant holds for every framework package (16 of them at audit
    //! time) without each package needing to re-prove it.
    use super::*;
    use fbuild_config::PackageOverride;

    const DEFAULT_URL: &str = "https://github.com/example/repo/archive/default.tar.gz";

    fn make_base(tmp: &Path, cache_root: &Path) -> PackageBase {
        PackageBase::with_cache_root(
            "framework-test",
            "0.1.0+gdefault",
            DEFAULT_URL,
            DEFAULT_URL,
            Some("0000000000000000000000000000000000000000000000000000000000000000"),
            CacheSubdir::Platforms,
            tmp,
            cache_root,
        )
    }

    #[test]
    fn override_changes_install_path() {
        // This is the unit test FastLED/fbuild#681 calls out by name:
        //   1. Default pin → some install_path P_default.
        //   2. Override URL A → install_path P_A, must differ from P_default.
        //   3. Override URL B (same repo, different commit) → install_path P_B,
        //      must differ from both P_default and P_A.
        //
        // If any two of these collide, a bisection step that swaps the URL
        // would silently reuse the previous commit's vendored sources — the
        // exact failure mode the override is meant to prevent.
        let tmp = tempfile::TempDir::new().unwrap();
        let cache_root = tmp.path().join("cache");

        let default_base = make_base(tmp.path(), &cache_root);
        let p_default = default_base.install_path();

        let ovr_a = PackageOverride {
            url: "https://github.com/example/repo/archive/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.tar.gz"
                .to_string(),
            version: "0.0.0+gaaaaaaa".to_string(),
            checksum: None,
        };
        let p_a = make_base(tmp.path(), &cache_root)
            .with_override(ovr_a.clone())
            .install_path();

        let ovr_b = PackageOverride {
            url: "https://github.com/example/repo/archive/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.tar.gz"
                .to_string(),
            version: "0.0.0+gbbbbbbb".to_string(),
            checksum: None,
        };
        let p_b = make_base(tmp.path(), &cache_root)
            .with_override(ovr_b)
            .install_path();

        assert_ne!(
            p_default, p_a,
            "override URL must produce a different cache dir from the default pin"
        );
        assert_ne!(
            p_default, p_b,
            "second override URL must also differ from the default pin"
        );
        assert_ne!(
            p_a, p_b,
            "same upstream repo at different commits MUST hash to different cache dirs \
             — otherwise a bisection step silently reuses the previous commit's sources"
        );

        // Round-trip sanity: applying the same override twice yields the same path
        // (the hash is deterministic, not session-dependent).
        let p_a_again = make_base(tmp.path(), &cache_root)
            .with_override(ovr_a)
            .install_path();
        assert_eq!(p_a, p_a_again, "override hash must be deterministic");
    }

    #[test]
    fn override_replaces_url_cache_key_version_and_checksum() {
        // The cache-key swap is the load-bearing field — assert it directly so a
        // future refactor can't accidentally preserve the default `cache_key`
        // while updating only `url`.
        let tmp = tempfile::TempDir::new().unwrap();
        let cache_root = tmp.path().join("cache");
        let base = make_base(tmp.path(), &cache_root);
        assert_eq!(base.cache_key, DEFAULT_URL);
        assert!(base.checksum.is_some());

        let ovr = PackageOverride {
            url: "https://example.com/override/archive/cafef00d.tar.gz".to_string(),
            version: "0.0.0+gcafef00".to_string(),
            checksum: None,
        };
        let overridden = base.with_override(ovr.clone());
        assert_eq!(overridden.url, ovr.url);
        assert_eq!(
            overridden.cache_key, ovr.url,
            "cache_key MUST be set from the override URL — install_path() uses cache_key, not url"
        );
        assert_eq!(overridden.version, ovr.version);
        assert_eq!(overridden.checksum, None);
        assert_eq!(
            overridden.name, "framework-test",
            "name is preserved across override"
        );
    }
}
