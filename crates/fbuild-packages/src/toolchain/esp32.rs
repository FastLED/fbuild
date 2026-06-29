//! ESP32 toolchain package — RISC-V or Xtensa GCC from Espressif.
//!
//! Two toolchain families based on MCU architecture:
//! - **RISC-V** (`riscv32-esp-elf`): ESP32-C2/C3/C5/C6/H2/P4
//! - **Xtensa** (`xtensa-esp-elf`): ESP32/S2/S3
//!
//! Toolchain binaries are downloaded from Espressif's GitHub releases.

use std::path::{Path, PathBuf};

use crate::{CacheSubdir, PackageBase, PackageInfo, Toolchain};

/// ESP32 toolchain version (matches pioarduino).
const ESP32_TOOLCHAIN_VERSION: &str = "14.2.0_20241119";

/// ESP32 toolchain manager.
///
/// Handles both RISC-V and Xtensa toolchains based on MCU architecture.
pub struct Esp32Toolchain {
    base: PackageBase,
    install_dir: Option<PathBuf>,
    /// Binary prefix: "riscv32-esp-elf-" or "xtensa-esp-elf-"
    prefix: String,
}

impl Esp32Toolchain {
    /// Create a new ESP32 toolchain (legacy, hardcoded URLs — used by tests).
    ///
    /// - `is_riscv`: true for C2/C3/C5/C6/P4, false for ESP32/S3
    /// - `prefix`: "riscv32-esp-elf-" or "xtensa-esp-elf-"
    pub fn new(project_dir: &Path, is_riscv: bool, prefix: &str) -> Self {
        let (url, checksum) = platform_package(is_riscv);
        let name = if is_riscv {
            "esp32-riscv-gcc"
        } else {
            "esp32-xtensa-gcc"
        };

        Self {
            base: PackageBase::new(
                name,
                ESP32_TOOLCHAIN_VERSION,
                &url,
                &url,
                checksum.as_deref(),
                CacheSubdir::Toolchains,
                project_dir,
            ),
            install_dir: None,
            prefix: prefix.to_string(),
        }
    }

    #[cfg(test)]
    fn with_cache_root(
        project_dir: &Path,
        cache_root: &Path,
        is_riscv: bool,
        prefix: &str,
    ) -> Self {
        let (url, checksum) = platform_package(is_riscv);
        let name = if is_riscv {
            "esp32-riscv-gcc"
        } else {
            "esp32-xtensa-gcc"
        };
        Self {
            base: PackageBase::with_cache_root(
                name,
                ESP32_TOOLCHAIN_VERSION,
                &url,
                &url,
                checksum.as_deref(),
                CacheSubdir::Toolchains,
                project_dir,
                cache_root,
            ),
            install_dir: None,
            prefix: prefix.to_string(),
        }
    }

    /// Create an ESP32 toolchain from a metadata-resolved URL.
    ///
    /// This is the preferred constructor — the orchestrator resolves the
    /// actual download URL from platform.json → tools.json, then passes
    /// the resolved URL and SHA256 here.
    pub fn from_resolved(
        project_dir: &Path,
        url: &str,
        checksum: Option<&str>,
        is_riscv: bool,
        prefix: &str,
    ) -> Self {
        let name = if is_riscv {
            "esp32-riscv-gcc"
        } else {
            "esp32-xtensa-gcc"
        };

        // Use the toolchain name as cache key for stability across URL changes
        let cache_key = if is_riscv {
            "toolchain-riscv32-esp"
        } else {
            "toolchain-xtensa-esp-elf"
        };

        // Extract a version string from the URL (e.g., "14.2.0_20241119")
        let version = extract_version_from_url(url);

        Self {
            base: PackageBase::new(
                name,
                &version,
                url,
                cache_key,
                checksum,
                CacheSubdir::Toolchains,
                project_dir,
            ),
            install_dir: None,
            prefix: prefix.to_string(),
        }
    }

    /// Get the resolved install directory, or compute it.
    fn resolved_dir(&self) -> PathBuf {
        self.install_dir
            .clone()
            .unwrap_or_else(|| find_bin_root(&self.base.install_path()))
    }

    /// Validate the toolchain installation.
    fn validate_install(install_dir: &Path, prefix: &str) -> fbuild_core::Result<()> {
        let root = find_bin_root(install_dir);
        let bin_dir = root.join("bin");

        if !bin_dir.exists() {
            return Err(fbuild_core::FbuildError::PackageError(format!(
                "ESP32 toolchain bin directory not found at {}",
                bin_dir.display()
            )));
        }

        let required_tools = ["gcc", "g++", "ar", "objcopy", "size"];
        for tool in &required_tools {
            let tool_path = tool_binary(&bin_dir, &format!("{}{}", prefix, tool));
            if !tool_path.exists() {
                return Err(fbuild_core::FbuildError::PackageError(format!(
                    "required tool {}{} not found at {}",
                    prefix,
                    tool,
                    tool_path.display()
                )));
            }
        }

        Ok(())
    }
}

#[async_trait::async_trait]
impl crate::Package for Esp32Toolchain {
    async fn ensure_installed(&self) -> fbuild_core::Result<PathBuf> {
        if self.is_installed() {
            return Ok(self.resolved_dir());
        }

        let prefix = self.prefix.clone();
        let install_path = self
            .base
            .staged_install(|dir| Self::validate_install(dir, &prefix))
            .await?;

        Ok(find_bin_root(&install_path))
    }

    fn is_installed(&self) -> bool {
        if !self.base.is_cached() {
            return false;
        }
        let root = find_bin_root(&self.base.install_path());
        root.join("bin")
            .join(tool_name(&format!("{}gcc", self.prefix)))
            .exists()
    }

    fn get_info(&self) -> PackageInfo {
        self.base.get_info()
    }
}

impl Toolchain for Esp32Toolchain {
    fn get_gcc_path(&self) -> PathBuf {
        tool_binary(
            &self.resolved_dir().join("bin"),
            &format!("{}gcc", self.prefix),
        )
    }

    fn get_gxx_path(&self) -> PathBuf {
        tool_binary(
            &self.resolved_dir().join("bin"),
            &format!("{}g++", self.prefix),
        )
    }

    fn get_ar_path(&self) -> PathBuf {
        tool_binary(
            &self.resolved_dir().join("bin"),
            &format!("{}ar", self.prefix),
        )
    }

    fn get_objcopy_path(&self) -> PathBuf {
        tool_binary(
            &self.resolved_dir().join("bin"),
            &format!("{}objcopy", self.prefix),
        )
    }

    fn get_size_path(&self) -> PathBuf {
        tool_binary(
            &self.resolved_dir().join("bin"),
            &format!("{}size", self.prefix),
        )
    }

    fn get_bin_dir(&self) -> PathBuf {
        self.resolved_dir().join("bin")
    }
}

/// A platform-specific ESP32 toolchain package entry.
struct Esp32PlatformPackage {
    filename_suffix: &'static str,
    /// SHA-256 checksum. `None` = skip verification (not yet captured).
    checksum: Option<&'static str>,
}

/// All platform variants for the ESP32 toolchain — no `cfg!` so tests can
/// validate every entry regardless of which platform runs the test.
fn all_platform_packages() -> [(&'static str, Esp32PlatformPackage); 5] {
    [
        (
            "windows",
            Esp32PlatformPackage {
                filename_suffix: "win64.zip",
                // TODO: capture real checksum from Windows CI run
                checksum: None,
            },
        ),
        (
            "macos-arm64",
            Esp32PlatformPackage {
                filename_suffix: "macos-arm64.tar.xz",
                // TODO: capture real checksum from macOS ARM CI run
                checksum: None,
            },
        ),
        (
            "macos-x86_64",
            Esp32PlatformPackage {
                filename_suffix: "macos.tar.xz",
                // TODO: capture real checksum from macOS x86_64 CI run
                checksum: None,
            },
        ),
        (
            "linux-aarch64",
            Esp32PlatformPackage {
                filename_suffix: "linux-arm64.tar.xz",
                // TODO: capture real checksum from aarch64 CI run
                checksum: None,
            },
        ),
        (
            "linux-x86_64",
            Esp32PlatformPackage {
                filename_suffix: "linux-amd64.tar.xz",
                // TODO: capture real checksum from Linux x86_64 CI run
                checksum: None,
            },
        ),
    ]
}

/// Get the platform-specific download URL and optional checksum for RISC-V or Xtensa.
fn platform_package(is_riscv: bool) -> (String, Option<String>) {
    let arch_name = if is_riscv {
        "riscv32-esp-elf"
    } else {
        "xtensa-esp-elf"
    };

    let key = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            "macos-arm64"
        } else {
            "macos-x86_64"
        }
    } else if cfg!(target_arch = "aarch64") {
        "linux-aarch64"
    } else {
        "linux-x86_64"
    };

    let packages = all_platform_packages();
    let (_, pkg) = packages
        .iter()
        .find(|(k, _)| *k == key)
        .expect("no ESP32 package for current platform");

    let url = format!(
        "https://github.com/espressif/crosstool-NG/releases/download/esp-{}/{}-{}.{}",
        ESP32_TOOLCHAIN_VERSION, arch_name, ESP32_TOOLCHAIN_VERSION, pkg.filename_suffix,
    );

    (url, pkg.checksum.map(|s| s.to_string()))
}

/// Extract a version-like string from a toolchain URL.
///
/// Looks for patterns like `14.2.0_20241119` in the URL path segments.
/// Falls back to a URL hash if no version pattern is found.
fn extract_version_from_url(url: &str) -> String {
    // Try to find a segment that looks like a version (contains digits and dots/underscores)
    for segment in url.rsplit('/') {
        let segment = segment
            .trim_end_matches(".zip")
            .trim_end_matches(".tar.xz")
            .trim_end_matches(".tar.gz")
            .trim_end_matches(".tar.bz2");
        // Look for version-like patterns: digits with dots, dashes, or underscores
        if segment.contains('.')
            && segment.chars().any(|c| c.is_ascii_digit())
            && segment.len() < 80
        {
            return segment.to_string();
        }
    }
    // Fallback: hash the URL
    crate::cache::hash_url(url)
}

/// Find the actual root directory containing bin/ inside an extracted archive.
fn find_bin_root(install_dir: &Path) -> PathBuf {
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

    #[test]
    fn test_platform_package_riscv() {
        let (url, _checksum) = platform_package(true);
        assert!(url.contains("riscv32-esp-elf"));
        assert!(url.contains("espressif"));
    }

    #[test]
    fn test_platform_package_xtensa() {
        let (url, _checksum) = platform_package(false);
        assert!(url.contains("xtensa-esp-elf"));
        assert!(url.contains("espressif"));
    }

    #[test]
    fn test_tool_name_platform() {
        let name = tool_name("riscv32-esp-elf-gcc");
        if cfg!(windows) {
            assert_eq!(name, "riscv32-esp-elf-gcc.exe");
        } else {
            assert_eq!(name, "riscv32-esp-elf-gcc");
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
        let nested = tmp.path().join("riscv32-esp-elf");
        std::fs::create_dir_all(nested.join("bin")).unwrap();
        assert_eq!(find_bin_root(tmp.path()), nested);
    }

    #[test]
    fn test_esp32_toolchain_not_installed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tc = Esp32Toolchain::with_cache_root(
            tmp.path(),
            &tmp.path().join("cache"),
            true,
            "riscv32-esp-elf-",
        );
        assert!(!tc.is_installed());
    }

    #[test]
    fn test_esp32_toolchain_riscv_prefix() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tc = Esp32Toolchain::new(tmp.path(), true, "riscv32-esp-elf-");
        assert_eq!(tc.prefix, "riscv32-esp-elf-");
    }

    #[test]
    fn test_esp32_toolchain_xtensa_prefix() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tc = Esp32Toolchain::new(tmp.path(), false, "xtensa-esp-elf-");
        assert_eq!(tc.prefix, "xtensa-esp-elf-");
    }

    #[test]
    fn test_esp32_toolchain_tools() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tc = Esp32Toolchain::new(tmp.path(), true, "riscv32-esp-elf-");
        let tools = tc.get_all_tools();
        assert!(tools.contains_key("gcc"));
        assert!(tools.contains_key("g++"));
        assert!(tools.contains_key("ar"));
        assert!(tools.contains_key("objcopy"));
        assert!(tools.contains_key("size"));
        // Verify RISC-V prefix in paths
        let gcc = tools.get("gcc").unwrap().to_string_lossy().to_string();
        assert!(gcc.contains("riscv32-esp-elf-gcc"));
    }

    /// Checksums that are present must be valid 64-char lowercase hex (SHA-256).
    /// Catches placeholder hashes like `a000...0010` on all platforms.
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
                assert!(
                    !hash.starts_with("a000000"),
                    "checksum for {platform} looks like a placeholder: {hash}",
                );
            }
        }
    }

    /// Every platform entry must have a valid filename suffix.
    #[test]
    fn test_all_platform_suffixes_are_valid() {
        let valid_suffixes = [
            "win64.zip",
            "macos-arm64.tar.xz",
            "macos.tar.xz",
            "linux-arm64.tar.xz",
            "linux-amd64.tar.xz",
        ];
        for (platform, pkg) in &all_platform_packages() {
            assert!(
                valid_suffixes.contains(&pkg.filename_suffix),
                "unexpected filename suffix for {platform}: {}",
                pkg.filename_suffix,
            );
        }
    }
}
