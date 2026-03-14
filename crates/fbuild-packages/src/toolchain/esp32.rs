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
                Some(&checksum),
                CacheSubdir::Toolchains,
                project_dir,
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

impl crate::Package for Esp32Toolchain {
    fn ensure_installed(&self) -> fbuild_core::Result<PathBuf> {
        if self.is_installed() {
            return Ok(self.resolved_dir());
        }

        let prefix = self.prefix.clone();
        let rt = tokio::runtime::Handle::try_current().ok();
        let install_path = if let Some(handle) = rt {
            handle.block_on(
                self.base
                    .staged_install(|dir| Self::validate_install(dir, &prefix)),
            )?
        } else {
            let rt = tokio::runtime::Runtime::new().map_err(|e| {
                fbuild_core::FbuildError::PackageError(format!(
                    "failed to create tokio runtime: {}",
                    e
                ))
            })?;
            rt.block_on(
                self.base
                    .staged_install(|dir| Self::validate_install(dir, &prefix)),
            )?
        };

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

/// Get the platform-specific download URL and checksum for RISC-V or Xtensa.
fn platform_package(is_riscv: bool) -> (String, String) {
    let arch_name = if is_riscv {
        "riscv32-esp-elf"
    } else {
        "xtensa-esp-elf"
    };

    let (filename_suffix, checksum) = if cfg!(target_os = "windows") {
        (
            "win64.zip",
            // TODO: verify Windows checksum after first download
            "a000000000000000000000000000000000000000000000000000000000000010",
        )
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            (
                "macos-arm64.tar.xz",
                // TODO: verify macOS ARM checksum
                "a000000000000000000000000000000000000000000000000000000000000011",
            )
        } else {
            (
                "macos.tar.xz",
                // TODO: verify macOS x86_64 checksum
                "a000000000000000000000000000000000000000000000000000000000000012",
            )
        }
    } else if cfg!(target_arch = "aarch64") {
        (
            "linux-arm64.tar.xz",
            // TODO: verify Linux ARM64 checksum
            "a000000000000000000000000000000000000000000000000000000000000013",
        )
    } else {
        (
            "linux-amd64.tar.xz",
            // TODO: verify Linux x86_64 checksum
            "a000000000000000000000000000000000000000000000000000000000000014",
        )
    };

    let url = format!(
        "https://github.com/espressif/crosstool-NG/releases/download/esp-{}/{}-{}.{}",
        ESP32_TOOLCHAIN_VERSION, arch_name, ESP32_TOOLCHAIN_VERSION, filename_suffix,
    );

    (url, checksum.to_string())
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
        let (url, checksum) = platform_package(true);
        assert!(url.contains("riscv32-esp-elf"));
        assert!(url.contains("espressif"));
        assert_eq!(checksum.len(), 64);
    }

    #[test]
    fn test_platform_package_xtensa() {
        let (url, checksum) = platform_package(false);
        assert!(url.contains("xtensa-esp-elf"));
        assert!(url.contains("espressif"));
        assert_eq!(checksum.len(), 64);
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
        std::env::set_var("FBUILD_CACHE_DIR", tmp.path().join("cache"));
        let tc = Esp32Toolchain::new(tmp.path(), true, "riscv32-esp-elf-");
        assert!(!tc.is_installed());
        std::env::remove_var("FBUILD_CACHE_DIR");
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
}
