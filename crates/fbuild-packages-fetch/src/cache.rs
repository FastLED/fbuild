//! Cache management for packages, toolchains, platforms, and libraries.
//!
//! Cache structure (stem/hash format for human-readable browsing):
//! ```text
//! ~/.fbuild/{dev|prod}/cache/
//!   packages/{stem}/{hash}/{version}/{filename}
//!   toolchains/{stem}/{hash}/{version}/
//!   platforms/{stem}/{hash}/{version}/
//!   libraries/{stem}/{hash}/{version}/
//!   core/{hash}/
//!   framework-libs/{hash}/
//!
//! <project>/.fbuild/build/{env}/{profile}/
//!   core/   (compiled core .o files)
//!   src/    (compiled sketch .o files)
//! ```
//!
//! The `stem` is a human-readable name derived from the URL.
//! The `hash` is the first 16 chars of SHA256 for uniqueness.

use fbuild_core::BuildProfile;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Manages cache directories for packages and build artifacts.
pub struct Cache {
    /// Root of the global cache (e.g. ~/.fbuild/prod/cache)
    cache_root: PathBuf,
    /// Project directory
    project_dir: PathBuf,
}

impl Cache {
    /// Create a new Cache for the given project directory.
    pub fn new(project_dir: &Path) -> Self {
        Self {
            cache_root: fbuild_paths::get_cache_root(),
            project_dir: project_dir.to_path_buf(),
        }
    }

    /// Create a Cache with a custom cache root (for testing).
    pub fn with_cache_root(project_dir: &Path, cache_root: &Path) -> Self {
        Self {
            cache_root: cache_root.to_path_buf(),
            project_dir: project_dir.to_path_buf(),
        }
    }

    // --- Global cache directories ---

    pub fn packages_dir(&self) -> PathBuf {
        self.cache_root.join("packages")
    }

    pub fn toolchains_dir(&self) -> PathBuf {
        self.cache_root.join("toolchains")
    }

    pub fn platforms_dir(&self) -> PathBuf {
        self.cache_root.join("platforms")
    }

    pub fn libraries_dir(&self) -> PathBuf {
        self.cache_root.join("libraries")
    }

    pub fn core_artifacts_dir(&self) -> PathBuf {
        self.cache_root.join("core")
    }

    /// Reusable framework-supplied library archives, keyed by their complete
    /// compilation input signature. These are deliberately separate from
    /// project `lib/` outputs: a normal clean removes only project artifacts.
    pub fn framework_library_artifacts_dir(&self) -> PathBuf {
        self.cache_root.join("framework-libs")
    }

    // --- Package path resolution (stem/hash) ---

    /// Get the cache path for a package URL + version.
    pub fn get_package_path(&self, url: &str, version: &str) -> PathBuf {
        let stem = url_stem(url);
        let hash = hash_url(url);
        self.packages_dir().join(stem).join(hash).join(version)
    }

    /// Get the cache path for a toolchain URL + version.
    pub fn get_toolchain_path(&self, url: &str, version: &str) -> PathBuf {
        let stem = url_stem(url);
        let hash = hash_url(url);
        self.toolchains_dir().join(stem).join(hash).join(version)
    }

    /// Get the cache path for a platform URL + version.
    pub fn get_platform_path(&self, url: &str, version: &str) -> PathBuf {
        let stem = url_stem(url);
        let hash = hash_url(url);
        self.platforms_dir().join(stem).join(hash).join(version)
    }

    // --- Cache existence checks ---

    pub fn is_package_cached(&self, url: &str, version: &str) -> bool {
        self.get_package_path(url, version).exists()
    }

    pub fn is_toolchain_cached(&self, url: &str, version: &str) -> bool {
        let path = self.get_toolchain_path(url, version);
        path.exists() && path.is_dir()
    }

    pub fn is_platform_cached(&self, url: &str, version: &str) -> bool {
        self.get_platform_path(url, version).exists()
    }

    // --- Build directories (per-project) ---

    /// Get the build directory for an environment and profile.
    ///
    /// Routes through [`fbuild_paths::BuildLayout`] so layout decisions
    /// (env-segment collapse when the project basename already matches
    /// the env, `FBUILD_BUILD_DIR` override, etc.) are made in exactly
    /// one place. Embedders that need a non-default layout should set
    /// `params.build_dir` directly via [`fbuild_paths::BuildLayout`];
    /// this convenience helper is for the default path only.
    pub fn get_build_dir(&self, env_name: &str, profile: BuildProfile) -> PathBuf {
        fbuild_paths::BuildLayout::new(self.project_dir.clone(), env_name.to_string(), profile)
            .resolve()
    }

    /// Get the core build subdirectory (for compiled core .o files).
    pub fn get_core_build_dir(&self, env_name: &str, profile: BuildProfile) -> PathBuf {
        self.get_build_dir(env_name, profile).join("core")
    }

    /// Get the src build subdirectory (for compiled sketch .o files).
    pub fn get_src_build_dir(&self, env_name: &str, profile: BuildProfile) -> PathBuf {
        self.get_build_dir(env_name, profile).join("src")
    }

    // --- Directory management ---

    /// Ensure all global cache directories exist.
    pub fn ensure_directories(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(self.packages_dir())?;
        std::fs::create_dir_all(self.toolchains_dir())?;
        std::fs::create_dir_all(self.platforms_dir())?;
        std::fs::create_dir_all(self.libraries_dir())?;
        std::fs::create_dir_all(self.core_artifacts_dir())?;
        std::fs::create_dir_all(self.framework_library_artifacts_dir())?;
        Ok(())
    }

    /// Ensure build directories exist for an environment and profile.
    pub fn ensure_build_directories(
        &self,
        env_name: &str,
        profile: BuildProfile,
    ) -> std::io::Result<()> {
        std::fs::create_dir_all(self.get_core_build_dir(env_name, profile))?;
        std::fs::create_dir_all(self.get_src_build_dir(env_name, profile))?;
        Ok(())
    }

    /// Get a toolchain metadata cache directory by name.
    ///
    /// Returns `{cache_root}/toolchains/{toolchain_name}` — used by
    /// `esp32_metadata::resolve_toolchain_url_sync` to store `tools.json`.
    ///
    /// `toolchain_name` is validated to be a single normal path component
    /// (no `/`, `\`, `..`, or absolute paths) to prevent directory traversal.
    pub fn toolchain_metadata_dir(&self, toolchain_name: &str) -> PathBuf {
        use std::path::Component;
        let path = Path::new(toolchain_name);
        let mut comps = path.components();
        let is_safe = matches!(comps.next(), Some(Component::Normal(_))) && comps.next().is_none();
        if is_safe {
            self.toolchains_dir().join(toolchain_name)
        } else {
            // Unsafe name — hash it to a safe single component
            let safe_name = format!("_unsafe_{}", hash_url(toolchain_name));
            self.toolchains_dir().join(safe_name)
        }
    }

    /// Clean build directory for an environment and profile.
    pub fn clean_build(&self, env_name: &str, profile: BuildProfile) -> std::io::Result<()> {
        let build_dir = self.get_build_dir(env_name, profile);
        if build_dir.exists() {
            std::fs::remove_dir_all(&build_dir)?;
        }
        Ok(())
    }
}

/// Hash a URL to a 16-character hex string for cache directory uniqueness.
pub fn hash_url(url: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(url.as_bytes());
    let result = hasher.finalize();
    hex_encode(&result[..8])
}

/// Extract a human-readable stem from a URL for cache directory naming.
///
/// Examples:
/// - `https://downloads.arduino.cc/tools` → `arduino-tools`
/// - `https://github.com/arduino/ArduinoCore-avr/archive/refs/tags/1.8.6.tar.gz` → `arduino-ArduinoCore-avr`
/// - `https://github.com/FastLED/FastLED#master` → `FastLED-FastLED`
/// - `https://example.com/some/deep/path/package.tar.gz` → `example.com-package`
pub fn url_stem(url: &str) -> String {
    // Strip protocol
    let without_proto = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);

    // Strip fragment (#branch)
    let without_frag = without_proto.split('#').next().unwrap_or(without_proto);

    // Strip query string
    let without_query = without_frag.split('?').next().unwrap_or(without_frag);

    // Split into host and path
    let (host, path) = match without_query.find('/') {
        Some(pos) => (&without_query[..pos], &without_query[pos + 1..]),
        None => (without_query, ""),
    };

    // For GitHub URLs: use org/repo pattern
    if host == "github.com" {
        let parts: Vec<&str> = path.split('/').collect();
        if parts.len() >= 2 {
            let org = parts[0];
            let repo = parts[1];
            return sanitize_stem(&format!("{}-{}", org, repo));
        }
    }

    // For other URLs: use last meaningful path segment
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    let last = segments
        .last()
        .copied()
        .unwrap_or("")
        // Strip archive extensions
        .trim_end_matches(".tar.gz")
        .trim_end_matches(".tar.bz2")
        .trim_end_matches(".tar.xz")
        .trim_end_matches(".zip")
        .trim_end_matches(".tgz");

    // Simplify host: strip common prefixes
    let short_host = host
        .strip_prefix("www.")
        .unwrap_or(host)
        .strip_prefix("downloads.")
        .unwrap_or(host);

    // Extract domain name without TLD for brevity
    let domain = short_host.split('.').next().unwrap_or(short_host);

    if last.is_empty() || last == domain {
        sanitize_stem(short_host)
    } else {
        sanitize_stem(&format!("{}-{}", domain, last))
    }
}

/// Sanitize a string for use as a directory name.
fn sanitize_stem(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{:02x}", b);
            s
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn tempdir() -> TempDir {
        TempDir::new_in(fbuild_paths::temp_subdir("fbuild-packages-cache-tests")).unwrap()
    }

    #[test]
    fn test_hash_url_deterministic() {
        let h1 = hash_url("https://example.com/package.tar.gz");
        let h2 = hash_url("https://example.com/package.tar.gz");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 16);
    }

    #[test]
    fn test_hash_url_different_urls() {
        let h1 = hash_url("https://example.com/a.tar.gz");
        let h2 = hash_url("https://example.com/b.tar.gz");
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_url_stem_arduino_tools() {
        assert_eq!(
            url_stem("https://downloads.arduino.cc/tools"),
            "arduino-tools"
        );
    }

    #[test]
    fn test_url_stem_github_repo() {
        assert_eq!(
            url_stem("https://github.com/arduino/ArduinoCore-avr/archive/refs/tags/1.8.6.tar.gz"),
            "arduino-ArduinoCore-avr"
        );
    }

    #[test]
    fn test_url_stem_github_fastled() {
        assert_eq!(
            url_stem("https://github.com/FastLED/FastLED#master"),
            "FastLED-FastLED"
        );
    }

    #[test]
    fn test_url_stem_plain_url() {
        assert_eq!(
            url_stem("https://example.com/some/path/package.tar.gz"),
            "example-package"
        );
    }

    #[test]
    fn test_packages_dir() {
        let tmp = tempdir();
        let cache = Cache::with_cache_root(tmp.path(), tmp.path().join("cache").as_path());
        assert!(cache.packages_dir().ends_with("packages"));
    }

    #[test]
    fn test_toolchains_dir() {
        let tmp = tempdir();
        let cache = Cache::with_cache_root(tmp.path(), tmp.path().join("cache").as_path());
        assert!(cache.toolchains_dir().ends_with("toolchains"));
    }

    #[test]
    fn global_artifact_roots_stay_under_authoritative_cache_root() {
        let tmp = tempdir();
        let cache_root = tmp.path().join("cache-root");
        let cache = Cache::with_cache_root(tmp.path(), cache_root.as_path());

        assert_eq!(cache.packages_dir(), cache_root.join("packages"));
        assert_eq!(cache.toolchains_dir(), cache_root.join("toolchains"));
        assert_eq!(cache.platforms_dir(), cache_root.join("platforms"));
        assert_eq!(cache.libraries_dir(), cache_root.join("libraries"));
        assert_eq!(cache.core_artifacts_dir(), cache_root.join("core"));
        assert_eq!(
            cache.framework_library_artifacts_dir(),
            cache_root.join("framework-libs")
        );
        assert!(
            cache
                .get_package_path("https://example.test/pkg.tar.gz", "1.0.0")
                .starts_with(cache_root.join("packages"))
        );
        assert!(
            cache
                .get_toolchain_path("https://example.test/tool.tar.gz", "1.0.0")
                .starts_with(cache_root.join("toolchains"))
        );
        assert!(
            cache
                .get_platform_path("https://example.test/platform.tar.gz", "1.0.0")
                .starts_with(cache_root.join("platforms"))
        );
    }

    #[test]
    fn test_get_package_path_with_stem_hash() {
        let tmp = tempdir();
        let cache = Cache::with_cache_root(tmp.path(), tmp.path().join("cache").as_path());
        let path = cache.get_package_path("https://example.com/pkg.tar.gz", "1.0.0");
        let path_str = path.to_string_lossy();
        assert!(path_str.contains("packages"));
        assert!(path_str.contains("example-pkg")); // stem
        assert!(path_str.contains("1.0.0")); // version
    }

    #[test]
    fn test_get_toolchain_path_with_stem_hash() {
        let tmp = tempdir();
        let cache = Cache::with_cache_root(tmp.path(), tmp.path().join("cache").as_path());
        let path = cache.get_toolchain_path("https://downloads.arduino.cc/tools", "7.3.0");
        let path_str = path.to_string_lossy();
        assert!(path_str.contains("toolchains"));
        assert!(path_str.contains("arduino-tools")); // stem
        assert!(path_str.contains("08e1a7271edb2765")); // hash
        assert!(path_str.contains("7.3.0")); // version
    }

    #[test]
    fn test_get_build_dir() {
        let tmp = tempdir();
        let cache = Cache::new(tmp.path());
        let release_dir = cache.get_build_dir("uno", BuildProfile::Release);
        assert!(release_dir.to_string_lossy().contains("uno"));
        assert!(release_dir.to_string_lossy().contains("release"));
        let quick_dir = cache.get_build_dir("uno", BuildProfile::Quick);
        assert!(quick_dir.to_string_lossy().contains("uno"));
        assert!(quick_dir.to_string_lossy().contains("quick"));
    }

    #[test]
    fn test_get_core_build_dir() {
        let tmp = tempdir();
        let cache = Cache::new(tmp.path());
        let dir = cache.get_core_build_dir("uno", BuildProfile::Release);
        assert!(dir.ends_with("core"));
    }

    #[test]
    fn test_get_src_build_dir() {
        let tmp = tempdir();
        let cache = Cache::new(tmp.path());
        let dir = cache.get_src_build_dir("uno", BuildProfile::Release);
        assert!(dir.ends_with("src"));
    }

    #[test]
    fn test_ensure_directories() {
        let tmp = tempdir();
        let cache = Cache::with_cache_root(tmp.path(), tmp.path().join("cache").as_path());
        cache.ensure_directories().unwrap();
        assert!(cache.packages_dir().exists());
        assert!(cache.toolchains_dir().exists());
        assert!(cache.platforms_dir().exists());
        assert!(cache.libraries_dir().exists());
    }

    #[test]
    fn test_ensure_build_directories() {
        let tmp = tempdir();
        let cache = Cache::new(tmp.path());
        cache
            .ensure_build_directories("uno", BuildProfile::Release)
            .unwrap();
        assert!(
            cache
                .get_core_build_dir("uno", BuildProfile::Release)
                .exists()
        );
        assert!(
            cache
                .get_src_build_dir("uno", BuildProfile::Release)
                .exists()
        );
    }

    #[test]
    fn test_clean_build() {
        let tmp = tempdir();
        let cache = Cache::new(tmp.path());
        cache
            .ensure_build_directories("uno", BuildProfile::Release)
            .unwrap();
        assert!(cache.get_build_dir("uno", BuildProfile::Release).exists());
        cache.clean_build("uno", BuildProfile::Release).unwrap();
        assert!(!cache.get_build_dir("uno", BuildProfile::Release).exists());
    }

    #[test]
    fn test_clean_build_nonexistent() {
        let tmp = tempdir();
        let cache = Cache::new(tmp.path());
        cache
            .clean_build("nonexistent", BuildProfile::Release)
            .unwrap();
    }

    #[test]
    fn test_is_package_cached() {
        let tmp = tempdir();
        let cache = Cache::with_cache_root(tmp.path(), tmp.path().join("cache").as_path());
        let url = "https://example.com/pkg.tar.gz";
        assert!(!cache.is_package_cached(url, "1.0.0"));

        let path = cache.get_package_path(url, "1.0.0");
        std::fs::create_dir_all(&path).unwrap();
        assert!(cache.is_package_cached(url, "1.0.0"));
    }

    #[test]
    fn test_is_toolchain_cached() {
        let tmp = tempdir();
        let cache = Cache::with_cache_root(tmp.path(), tmp.path().join("cache").as_path());
        let url = "https://example.com/gcc.tar.gz";
        assert!(!cache.is_toolchain_cached(url, "7.3.0"));

        let path = cache.get_toolchain_path(url, "7.3.0");
        std::fs::create_dir_all(&path).unwrap();
        assert!(cache.is_toolchain_cached(url, "7.3.0"));
    }

    #[test]
    fn test_is_toolchain_cached_file_not_dir() {
        let tmp = tempdir();
        let cache = Cache::with_cache_root(tmp.path(), tmp.path().join("cache").as_path());
        let url = "https://example.com/gcc.tar.gz";
        let path = cache.get_toolchain_path(url, "7.3.0");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not a dir").unwrap();
        assert!(!cache.is_toolchain_cached(url, "7.3.0"));
    }

    #[test]
    fn test_multiple_environments() {
        let tmp = tempdir();
        let cache = Cache::new(tmp.path());
        cache
            .ensure_build_directories("uno", BuildProfile::Release)
            .unwrap();
        cache
            .ensure_build_directories("esp32", BuildProfile::Release)
            .unwrap();
        assert!(cache.get_build_dir("uno", BuildProfile::Release).exists());
        assert!(cache.get_build_dir("esp32", BuildProfile::Release).exists());
        cache.clean_build("uno", BuildProfile::Release).unwrap();
        assert!(!cache.get_build_dir("uno", BuildProfile::Release).exists());
        assert!(cache.get_build_dir("esp32", BuildProfile::Release).exists());
    }

    #[test]
    fn test_version_isolation() {
        let tmp = tempdir();
        let cache = Cache::with_cache_root(tmp.path(), tmp.path().join("cache").as_path());
        let url = "https://example.com/pkg.tar.gz";
        let v1 = cache.get_package_path(url, "1.0.0");
        let v2 = cache.get_package_path(url, "2.0.0");
        assert_ne!(v1, v2);
        // Same stem/hash, different version dirs
        assert_eq!(v1.parent().unwrap().parent(), v2.parent().unwrap().parent());
    }
}
