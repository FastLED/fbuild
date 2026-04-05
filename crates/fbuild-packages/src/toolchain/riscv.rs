//! RISC-V GCC toolchain package (xPack riscv-none-elf-gcc).
//!
//! Downloads and manages the xPack RISC-V GCC toolchain for CH32V and other
//! RISC-V embedded targets. Provides paths to: riscv-none-elf-gcc,
//! riscv-none-elf-g++, riscv-none-elf-ar, riscv-none-elf-objcopy,
//! riscv-none-elf-size.

use std::path::{Path, PathBuf};

use crate::{CacheSubdir, PackageBase, PackageInfo, Toolchain};

/// xPack RISC-V GCC toolchain version.
const RISCV_GCC_VERSION: &str = "14.2.0-3";
const RISCV_GCC_BASE_URL: &str =
    "https://github.com/xpack-dev-tools/riscv-none-elf-gcc-xpack/releases/download";

/// RISC-V GCC toolchain manager.
pub struct RiscvToolchain {
    base: PackageBase,
    /// Resolved install path (set after ensure_installed)
    install_dir: Option<PathBuf>,
}

impl RiscvToolchain {
    pub fn new(project_dir: &Path) -> Self {
        let (url, checksum) = platform_package();
        Self {
            base: PackageBase::new(
                "riscv-gcc",
                RISCV_GCC_VERSION,
                &url,
                RISCV_GCC_BASE_URL,
                checksum.as_deref(),
                CacheSubdir::Toolchains,
                project_dir,
            ),
            install_dir: None,
        }
    }

    #[cfg(test)]
    fn with_cache_root(project_dir: &Path, cache_root: &Path) -> Self {
        let (url, checksum) = platform_package();
        Self {
            base: PackageBase::with_cache_root(
                "riscv-gcc",
                RISCV_GCC_VERSION,
                &url,
                RISCV_GCC_BASE_URL,
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
                "riscv-gcc bin directory not found at {}",
                bin_dir.display()
            )));
        }

        let required_tools = [
            "riscv-none-elf-gcc",
            "riscv-none-elf-g++",
            "riscv-none-elf-ar",
            "riscv-none-elf-objcopy",
            "riscv-none-elf-size",
        ];
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

        Ok(())
    }
}

impl crate::Package for RiscvToolchain {
    fn ensure_installed(&self) -> fbuild_core::Result<PathBuf> {
        if self.is_installed() {
            return Ok(self.resolved_dir());
        }

        let rt = tokio::runtime::Handle::try_current().ok();
        let install_path = if let Some(handle) = rt {
            handle.block_on(self.base.staged_install(Self::validate))?
        } else {
            let rt = tokio::runtime::Runtime::new().map_err(|e| {
                fbuild_core::FbuildError::PackageError(format!(
                    "failed to create tokio runtime: {}",
                    e
                ))
            })?;
            rt.block_on(self.base.staged_install(Self::validate))?
        };

        Ok(find_bin_root(&install_path))
    }

    fn is_installed(&self) -> bool {
        if !self.base.is_cached() {
            return false;
        }
        let root = find_bin_root(&self.base.install_path());
        root.join("bin")
            .join(tool_name("riscv-none-elf-gcc"))
            .exists()
    }

    fn get_info(&self) -> PackageInfo {
        self.base.get_info()
    }
}

impl Toolchain for RiscvToolchain {
    fn get_gcc_path(&self) -> PathBuf {
        tool_binary(&self.resolved_dir().join("bin"), "riscv-none-elf-gcc")
    }

    fn get_gxx_path(&self) -> PathBuf {
        tool_binary(&self.resolved_dir().join("bin"), "riscv-none-elf-g++")
    }

    fn get_ar_path(&self) -> PathBuf {
        tool_binary(&self.resolved_dir().join("bin"), "riscv-none-elf-ar")
    }

    fn get_objcopy_path(&self) -> PathBuf {
        tool_binary(&self.resolved_dir().join("bin"), "riscv-none-elf-objcopy")
    }

    fn get_size_path(&self) -> PathBuf {
        tool_binary(&self.resolved_dir().join("bin"), "riscv-none-elf-size")
    }

    fn get_bin_dir(&self) -> PathBuf {
        self.resolved_dir().join("bin")
    }
}

/// A platform-specific RISC-V toolchain package entry.
struct RiscvPlatformPackage {
    filename: &'static str,
    /// SHA-256 checksum. `None` = skip verification (not yet captured).
    checksum: Option<&'static str>,
}

/// All platform variants for the RISC-V toolchain.
fn all_platform_packages() -> [(&'static str, RiscvPlatformPackage); 4] {
    [
        (
            "windows",
            RiscvPlatformPackage {
                filename: "xpack-riscv-none-elf-gcc-14.2.0-3-win32-x64.zip",
                // TODO: capture real checksum from Windows CI run
                checksum: None,
            },
        ),
        (
            "macos",
            RiscvPlatformPackage {
                filename: "xpack-riscv-none-elf-gcc-14.2.0-3-darwin-x64.tar.gz",
                // TODO: capture real checksum from macOS CI run
                checksum: None,
            },
        ),
        (
            "linux-aarch64",
            RiscvPlatformPackage {
                filename: "xpack-riscv-none-elf-gcc-14.2.0-3-linux-arm64.tar.gz",
                // TODO: capture real checksum from aarch64 CI run
                checksum: None,
            },
        ),
        (
            "linux-x86_64",
            RiscvPlatformPackage {
                filename: "xpack-riscv-none-elf-gcc-14.2.0-3-linux-x64.tar.gz",
                // TODO: capture real checksum from Linux x86_64 CI run
                checksum: None,
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
        .expect("no RISC-V package for current platform");

    (
        format!(
            "{}/v{}/{}",
            RISCV_GCC_BASE_URL, RISCV_GCC_VERSION, pkg.filename
        ),
        pkg.checksum.map(|s| s.to_string()),
    )
}

/// Find the actual root directory containing bin/ inside an extracted archive.
///
/// Archives often have a single top-level directory (e.g. `xpack-riscv-none-elf-gcc-.../`).
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
    use std::collections::HashMap;

    #[test]
    fn test_platform_package_returns_url() {
        let (url, _checksum) = platform_package();
        assert!(url.contains("xpack-dev-tools/riscv-none-elf-gcc-xpack"));
        assert!(url.contains("riscv-none-elf"));
    }

    #[test]
    fn test_tool_name_platform() {
        let name = tool_name("riscv-none-elf-gcc");
        if cfg!(windows) {
            assert_eq!(name, "riscv-none-elf-gcc.exe");
        } else {
            assert_eq!(name, "riscv-none-elf-gcc");
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
        let nested = tmp.path().join("xpack-riscv-none-elf-gcc-14.2.0-3");
        std::fs::create_dir_all(nested.join("bin")).unwrap();
        assert_eq!(find_bin_root(tmp.path()), nested);
    }

    #[test]
    fn test_riscv_toolchain_get_tools() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tc = RiscvToolchain::new(tmp.path());
        let tools: HashMap<String, PathBuf> = tc.get_all_tools();
        assert!(tools.contains_key("gcc"));
        assert!(tools.contains_key("g++"));
        assert!(tools.contains_key("ar"));
        assert!(tools.contains_key("objcopy"));
        assert!(tools.contains_key("size"));
    }

    #[test]
    fn test_riscv_toolchain_not_installed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tc = RiscvToolchain::with_cache_root(tmp.path(), &tmp.path().join("cache"));
        assert!(!tc.is_installed());
    }

    /// Every platform entry must have a valid URL.
    #[test]
    fn test_all_platform_urls_are_valid() {
        for (platform, pkg) in &all_platform_packages() {
            assert!(
                pkg.filename.contains("14.2.0-3"),
                "filename for {platform} doesn't contain version: {}",
                pkg.filename,
            );
        }
    }
}
