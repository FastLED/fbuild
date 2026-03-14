//! ARM GCC toolchain package.
//!
//! Downloads and manages the ARM GCC 15.2.Rel1 toolchain from developer.arm.com.
//! Provides paths to: arm-none-eabi-gcc, arm-none-eabi-g++, arm-none-eabi-ar,
//! arm-none-eabi-objcopy, arm-none-eabi-size.

use std::path::{Path, PathBuf};

use crate::{CacheSubdir, PackageBase, PackageInfo, Toolchain};

/// ARM GCC toolchain version.
const ARM_GCC_VERSION: &str = "15.2.Rel1";
const ARM_GCC_BASE_URL: &str = "https://developer.arm.com/-/media/Files/downloads/gnu";

/// ARM GCC toolchain manager.
pub struct ArmToolchain {
    base: PackageBase,
    /// Resolved install path (set after ensure_installed)
    install_dir: Option<PathBuf>,
}

impl ArmToolchain {
    pub fn new(project_dir: &Path) -> Self {
        let (url, checksum) = platform_package();
        Self {
            base: PackageBase::new(
                "arm-gcc",
                ARM_GCC_VERSION,
                &url,
                ARM_GCC_BASE_URL,
                Some(&checksum),
                CacheSubdir::Toolchains,
                project_dir,
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
                "arm-gcc bin directory not found at {}",
                bin_dir.display()
            )));
        }

        let required_tools = [
            "arm-none-eabi-gcc",
            "arm-none-eabi-g++",
            "arm-none-eabi-ar",
            "arm-none-eabi-objcopy",
            "arm-none-eabi-size",
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

impl crate::Package for ArmToolchain {
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
            .join(tool_name("arm-none-eabi-gcc"))
            .exists()
    }

    fn get_info(&self) -> PackageInfo {
        self.base.get_info()
    }
}

impl Toolchain for ArmToolchain {
    fn get_gcc_path(&self) -> PathBuf {
        tool_binary(&self.resolved_dir().join("bin"), "arm-none-eabi-gcc")
    }

    fn get_gxx_path(&self) -> PathBuf {
        tool_binary(&self.resolved_dir().join("bin"), "arm-none-eabi-g++")
    }

    fn get_ar_path(&self) -> PathBuf {
        tool_binary(&self.resolved_dir().join("bin"), "arm-none-eabi-ar")
    }

    fn get_objcopy_path(&self) -> PathBuf {
        tool_binary(&self.resolved_dir().join("bin"), "arm-none-eabi-objcopy")
    }

    fn get_size_path(&self) -> PathBuf {
        tool_binary(&self.resolved_dir().join("bin"), "arm-none-eabi-size")
    }

    fn get_bin_dir(&self) -> PathBuf {
        self.resolved_dir().join("bin")
    }
}

/// Get the platform-specific download URL and checksum.
fn platform_package() -> (String, String) {
    let (filename, checksum) = if cfg!(target_os = "windows") {
        (
            "arm-gnu-toolchain-15.2.rel1-mingw-w64-x86_64-arm-none-eabi.zip",
            "7936cac895611023ffb22a64b8e426098c7104cb689778c1894572ca840b9ece",
        )
    } else if cfg!(target_os = "macos") {
        (
            "arm-gnu-toolchain-15.2.rel1-darwin-x86_64-arm-none-eabi.tar.xz",
            // TODO: verify macOS checksum after first download
            "a000000000000000000000000000000000000000000000000000000000000002",
        )
    } else if cfg!(target_arch = "aarch64") {
        (
            "arm-gnu-toolchain-15.2.rel1-aarch64-arm-none-eabi.tar.xz",
            // TODO: verify aarch64 checksum after first download
            "a000000000000000000000000000000000000000000000000000000000000003",
        )
    } else {
        (
            "arm-gnu-toolchain-15.2.rel1-x86_64-arm-none-eabi.tar.xz",
            // TODO: verify Linux x86_64 checksum after first download
            "a000000000000000000000000000000000000000000000000000000000000004",
        )
    };

    (
        format!("{}/15.2.rel1/binrel/{}", ARM_GCC_BASE_URL, filename),
        checksum.to_string(),
    )
}

/// Find the actual root directory containing bin/ inside an extracted archive.
///
/// Archives often have a single top-level directory (e.g. `arm-gnu-toolchain-15.2.rel1-.../`).
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
    fn test_platform_package_returns_url_and_checksum() {
        let (url, checksum) = platform_package();
        assert!(url.starts_with("https://developer.arm.com"));
        assert!(url.contains("arm-none-eabi"));
        assert_eq!(checksum.len(), 64); // SHA256 hex
    }

    #[test]
    fn test_tool_name_platform() {
        let name = tool_name("arm-none-eabi-gcc");
        if cfg!(windows) {
            assert_eq!(name, "arm-none-eabi-gcc.exe");
        } else {
            assert_eq!(name, "arm-none-eabi-gcc");
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
        let nested = tmp.path().join("arm-gnu-toolchain-15.2.rel1");
        std::fs::create_dir_all(nested.join("bin")).unwrap();
        assert_eq!(find_bin_root(tmp.path()), nested);
    }

    #[test]
    fn test_arm_toolchain_get_tools() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tc = ArmToolchain::new(tmp.path());
        let tools: HashMap<String, PathBuf> = tc.get_all_tools();
        assert!(tools.contains_key("gcc"));
        assert!(tools.contains_key("g++"));
        assert!(tools.contains_key("ar"));
        assert!(tools.contains_key("objcopy"));
        assert!(tools.contains_key("size"));
    }

    #[test]
    fn test_arm_toolchain_not_installed() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::env::set_var("FBUILD_CACHE_DIR", tmp.path().join("cache"));
        let tc = ArmToolchain::new(tmp.path());
        assert!(!tc.is_installed());
        std::env::remove_var("FBUILD_CACHE_DIR");
    }
}
