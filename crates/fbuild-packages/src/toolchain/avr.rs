//! AVR-GCC toolchain package.
//!
//! Downloads and manages the avr-gcc 7.3.0 toolchain from Arduino's CDN.
//! Provides paths to: avr-gcc, avr-g++, avr-ar, avr-objcopy, avr-size.

use std::path::{Path, PathBuf};

use crate::{CacheSubdir, PackageBase, PackageInfo, Toolchain};

/// AVR-GCC toolchain version and download URLs.
const AVR_GCC_VERSION: &str = "7.3.0-atmel3.6.1-arduino7";
const AVR_GCC_BASE_URL: &str = "https://downloads.arduino.cc/tools";

/// AVR-GCC toolchain manager.
pub struct AvrToolchain {
    base: PackageBase,
    /// Resolved install path (set after ensure_installed)
    install_dir: Option<PathBuf>,
}

impl AvrToolchain {
    pub fn new(project_dir: &Path) -> Self {
        let (url, checksum) = platform_package();
        Self {
            base: PackageBase::new(
                "avr-gcc",
                AVR_GCC_VERSION,
                &url,
                AVR_GCC_BASE_URL,
                checksum.as_deref(),
                CacheSubdir::Toolchains,
                project_dir,
            ),
            install_dir: None,
        }
    }

    /// Create with an explicit cache root (for testing without env vars).
    #[cfg(test)]
    fn with_cache_root(project_dir: &Path, cache_root: &Path) -> Self {
        let (url, checksum) = platform_package();
        Self {
            base: PackageBase::with_cache_root(
                "avr-gcc",
                AVR_GCC_VERSION,
                &url,
                AVR_GCC_BASE_URL,
                checksum.as_deref(),
                CacheSubdir::Toolchains,
                project_dir,
                cache_root,
            ),
            install_dir: None,
        }
    }

    /// Get the resolved install directory, or compute it.
    fn resolved_dir(&self) -> PathBuf {
        self.install_dir
            .clone()
            .unwrap_or_else(|| find_bin_root(&self.base.install_path()))
    }

    /// Validate that the toolchain installation has all required files.
    fn validate(install_dir: &Path) -> fbuild_core::Result<()> {
        let root = find_bin_root(install_dir);
        let bin_dir = root.join("bin");

        if !bin_dir.exists() {
            return Err(fbuild_core::FbuildError::PackageError(format!(
                "avr-gcc bin directory not found at {}",
                bin_dir.display()
            )));
        }

        let required_tools = ["avr-gcc", "avr-g++", "avr-ar", "avr-objcopy", "avr-size"];
        for tool in &required_tools {
            let tool_path = tool_binary(&bin_dir, tool);
            if !tool_path.exists() {
                return Err(fbuild_core::FbuildError::PackageError(format!(
                    "required tool {} not found at {}",
                    tool,
                    tool_path.display()
                )));
            }
        }

        // Check for avr include headers
        let avr_include = root.join("avr").join("include");
        if !avr_include.exists() {
            // Some archives nest differently — check lib/avr/include
            let alt = root.join("lib").join("avr").join("include");
            if !alt.exists() {
                tracing::warn!(
                    "avr include directory not found (checked {} and {})",
                    avr_include.display(),
                    alt.display()
                );
            }
        }

        Ok(())
    }
}

#[async_trait::async_trait]
impl crate::Package for AvrToolchain {
    async fn ensure_installed(&self) -> fbuild_core::Result<PathBuf> {
        if self.is_installed() {
            return Ok(self.resolved_dir());
        }

        let install_path = self.base.staged_install(Self::validate).await?;
        Ok(find_bin_root(&install_path))
    }

    fn is_installed(&self) -> bool {
        if !self.base.is_cached() {
            return false;
        }
        let root = find_bin_root(&self.base.install_path());
        root.join("bin").join(tool_name("avr-gcc")).exists()
    }

    fn get_info(&self) -> PackageInfo {
        self.base.get_info()
    }
}

impl Toolchain for AvrToolchain {
    fn get_gcc_path(&self) -> PathBuf {
        tool_binary(&self.resolved_dir().join("bin"), "avr-gcc")
    }

    fn get_gxx_path(&self) -> PathBuf {
        tool_binary(&self.resolved_dir().join("bin"), "avr-g++")
    }

    fn get_ar_path(&self) -> PathBuf {
        tool_binary(&self.resolved_dir().join("bin"), "avr-ar")
    }

    fn get_objcopy_path(&self) -> PathBuf {
        tool_binary(&self.resolved_dir().join("bin"), "avr-objcopy")
    }

    fn get_size_path(&self) -> PathBuf {
        tool_binary(&self.resolved_dir().join("bin"), "avr-size")
    }

    fn get_bin_dir(&self) -> PathBuf {
        self.resolved_dir().join("bin")
    }
}

/// A platform-specific AVR toolchain package entry.
struct AvrPlatformPackage {
    filename: &'static str,
    /// SHA-256 checksum. `None` means skip verification (checksum not yet captured).
    checksum: Option<&'static str>,
}

/// All platform variants for the AVR toolchain.
///
/// Defined as a plain function (no `cfg!`) so tests can validate every entry
/// regardless of which platform the test runs on.
fn all_platform_packages() -> [(&'static str, AvrPlatformPackage); 4] {
    [
        (
            "windows",
            AvrPlatformPackage {
                filename: "avr-gcc-7.3.0-atmel3.6.1-arduino7-i686-w64-mingw32.zip",
                checksum: Some("a54f64755fff4cb792a1495e5defdd789902a2a3503982e81b898299cf39800e"),
            },
        ),
        (
            "macos",
            AvrPlatformPackage {
                filename: "avr-gcc-7.3.0-atmel3.6.1-arduino7-x86_64-apple-darwin14.tar.bz2",
                // TODO: capture real checksum from macOS CI run
                checksum: None,
            },
        ),
        (
            "linux-aarch64",
            AvrPlatformPackage {
                filename: "avr-gcc-7.3.0-atmel3.6.1-arduino7-aarch64-pc-linux-gnu.tar.bz2",
                // TODO: capture real checksum from aarch64 CI run
                checksum: None,
            },
        ),
        (
            "linux-x86_64",
            AvrPlatformPackage {
                filename: "avr-gcc-7.3.0-atmel3.6.1-arduino7-x86_64-pc-linux-gnu.tar.bz2",
                checksum: Some("bd8c37f6952a2130ac9ee32c53f6a660feb79bee8353c8e289eb60fdcefed91e"),
            },
        ),
    ]
}

/// Get the platform-specific download URL and optional checksum for the current host.
fn platform_package() -> (String, Option<String>) {
    let key = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_arch = "aarch64") {
        "linux-aarch64"
    } else {
        "linux-x86_64"
    };

    let packages = all_platform_packages();
    let (_, pkg) = packages
        .iter()
        .find(|(k, _)| *k == key)
        .expect("no AVR package for current platform");

    (
        format!("{}/{}", AVR_GCC_BASE_URL, pkg.filename),
        pkg.checksum.map(|s| s.to_string()),
    )
}

/// Find the actual root directory containing bin/ inside an extracted archive.
///
/// Archives often have a single top-level directory (e.g. `avr-gcc-7.3.0/`).
fn find_bin_root(install_dir: &Path) -> PathBuf {
    // Direct bin/ in install dir
    if install_dir.join("bin").exists() {
        return install_dir.to_path_buf();
    }

    // Check one level deep for a nested directory with bin/
    if let Ok(entries) = std::fs::read_dir(install_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && path.join("bin").exists() {
                return path;
            }
        }
    }

    // Fall back to install_dir itself
    install_dir.to_path_buf()
}

/// Get the tool binary name with .exe extension on Windows.
fn tool_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{}.exe", name)
    } else {
        name.to_string()
    }
}

/// Get the full path to a tool binary.
fn tool_binary(bin_dir: &Path, name: &str) -> PathBuf {
    bin_dir.join(tool_name(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Package;
    use std::collections::HashMap;

    #[test]
    fn test_platform_package_returns_url() {
        let (url, _checksum) = platform_package();
        assert!(url.starts_with("https://downloads.arduino.cc/tools/avr-gcc"));
    }

    /// Checksums that are present must be valid 64-char lowercase hex (SHA-256).
    /// This runs on every platform and checks ALL entries, catching placeholder
    /// hashes like the `5903f0f0...` pattern that broke Linux CI.
    #[test]
    fn test_all_checksums_are_valid_or_none() {
        for (platform, pkg) in &all_platform_packages() {
            if let Some(hash) = pkg.checksum {
                assert_eq!(
                    hash.len(),
                    64,
                    "checksum for {platform} has wrong length ({}): {hash}",
                    hash.len(),
                );
                assert!(
                    hash.chars().all(|c| c.is_ascii_hexdigit()),
                    "checksum for {platform} contains non-hex characters: {hash}",
                );
                // Reject obviously-fake incrementing patterns
                assert!(
                    !hash.contains("0aab8e3e6e5d5e15c5e2e0c8c"),
                    "checksum for {platform} looks like a placeholder: {hash}",
                );
            }
        }
    }

    /// Every platform entry must have a URL that starts with the base URL.
    #[test]
    fn test_all_platform_urls_are_valid() {
        for (platform, pkg) in &all_platform_packages() {
            let url = format!("{}/{}", AVR_GCC_BASE_URL, pkg.filename);
            assert!(
                url.starts_with("https://downloads.arduino.cc/tools/avr-gcc-"),
                "URL for {platform} doesn't start with expected prefix: {url}",
            );
            assert!(
                pkg.filename.contains(AVR_GCC_VERSION),
                "filename for {platform} doesn't contain version {AVR_GCC_VERSION}: {}",
                pkg.filename,
            );
        }
    }

    /// Windows and Linux x86_64 (the two CI platforms) must have checksums.
    /// macOS/aarch64 may be None until captured from a CI run.
    #[test]
    fn test_ci_platforms_have_checksums() {
        let packages = all_platform_packages();
        for (platform, pkg) in &packages {
            if *platform == "windows" || *platform == "linux-x86_64" {
                assert!(
                    pkg.checksum.is_some(),
                    "CI platform {platform} must have a checksum (not None)",
                );
            }
        }
    }

    /// Download the AVR toolchain for the current platform and verify the
    /// checksum matches what we have on file. Skipped when checksum is None.
    ///
    /// This is an integration test that hits the network. Run with:
    ///   cargo test -p fbuild-packages -- --ignored test_download_and_verify_checksum
    #[test]
    #[ignore]
    fn test_download_and_verify_checksum() {
        let (url, expected) = platform_package();
        let Some(expected) = expected else {
            eprintln!("skipping: no checksum for current platform (None)");
            return;
        };

        let tmp = tempfile::TempDir::new().unwrap();
        let out_path = tmp.path().join("avr-gcc.archive");

        // Blocking download (test uses standalone runtime since it's a sync #[test]).
        // `Runtime::new()` can fail (e.g. fd exhaustion); use `expect` so the
        // assertion shows up cleanly instead of a bare `unwrap` panic message.
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime should construct");
        rt.block_on(async {
            let resp = crate::http::client()
                .get(&url)
                .send()
                .await
                .expect("download failed");
            assert!(resp.status().is_success(), "HTTP {}", resp.status());
            let bytes = resp.bytes().await.expect("read body failed");
            std::fs::write(&out_path, &bytes).expect("write failed");
        });

        // Compute SHA-256
        use sha2::{Digest, Sha256};
        let data = std::fs::read(&out_path).unwrap();
        let hash = format!("{:x}", Sha256::digest(&data));

        assert_eq!(
            hash, expected,
            "checksum mismatch for current platform download\n\
             url: {url}\n\
             expected: {expected}\n\
             actual:   {hash}\n\
             If this fails, update the checksum in avr.rs"
        );
    }

    #[test]
    fn test_tool_name_platform() {
        let name = tool_name("avr-gcc");
        if cfg!(windows) {
            assert_eq!(name, "avr-gcc.exe");
        } else {
            assert_eq!(name, "avr-gcc");
        }
    }

    #[test]
    fn test_find_bin_root_direct() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("bin")).unwrap();
        assert_eq!(find_bin_root(tmp.path()), tmp.path().to_path_buf());
    }

    #[test]
    fn test_find_bin_root_nested() {
        let tmp = tempfile::TempDir::new().unwrap();
        let nested = tmp.path().join("avr-gcc-7.3.0");
        std::fs::create_dir_all(nested.join("bin")).unwrap();
        assert_eq!(find_bin_root(tmp.path()), nested);
    }

    #[test]
    fn test_avr_toolchain_get_tools() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tc = AvrToolchain::new(tmp.path());
        let tools: HashMap<String, PathBuf> = tc.get_all_tools();
        assert!(tools.contains_key("gcc"));
        assert!(tools.contains_key("g++"));
        assert!(tools.contains_key("ar"));
        assert!(tools.contains_key("objcopy"));
        assert!(tools.contains_key("size"));
    }

    #[test]
    fn test_avr_toolchain_not_installed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tc = AvrToolchain::with_cache_root(tmp.path(), &tmp.path().join("cache"));
        assert!(!tc.is_installed());
    }
}
