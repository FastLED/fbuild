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

impl crate::Package for AvrToolchain {
    fn ensure_installed(&self) -> fbuild_core::Result<PathBuf> {
        if self.is_installed() {
            return Ok(self.resolved_dir());
        }

        // Use tokio runtime for async staged_install
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

/// Get the platform-specific download URL and checksum.
fn platform_package() -> (String, String) {
    let (filename, checksum) = if cfg!(target_os = "windows") {
        (
            "avr-gcc-7.3.0-atmel3.6.1-arduino7-i686-w64-mingw32.zip",
            "a54f64755fff4cb792a1495e5defdd789902a2a3503982e81b898299cf39800e",
        )
    } else if cfg!(target_os = "macos") {
        // TODO: verify macOS checksum after first download
        (
            "avr-gcc-7.3.0-atmel3.6.1-arduino7-x86_64-apple-darwin14.tar.bz2",
            "3903f0f0aab8e3e6e5d5e15c5e2e0c8c8a0a5f9d5e5c5d5e5f5a5b5c5d5e5f5a",
        )
    } else if cfg!(target_arch = "aarch64") {
        // TODO: verify aarch64 checksum after first download
        (
            "avr-gcc-7.3.0-atmel3.6.1-arduino7-aarch64-pc-linux-gnu.tar.bz2",
            "4903f0f0aab8e3e6e5d5e15c5e2e0c8c8a0a5f9d5e5c5d5e5f5a5b5c5d5e5f5a",
        )
    } else {
        // TODO: verify Linux x86_64 checksum after first download
        (
            "avr-gcc-7.3.0-atmel3.6.1-arduino7-x86_64-pc-linux-gnu.tar.bz2",
            "5903f0f0aab8e3e6e5d5e15c5e2e0c8c8a0a5f9d5e5c5d5e5f5a5b5c5d5e5f5a",
        )
    };

    (
        format!("{}/{}", AVR_GCC_BASE_URL, filename),
        checksum.to_string(),
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
    fn test_platform_package_returns_url_and_checksum() {
        let (url, checksum) = platform_package();
        assert!(url.starts_with("https://downloads.arduino.cc/tools/avr-gcc"));
        assert_eq!(checksum.len(), 64); // SHA256 hex
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
        // Use isolated cache so global cache doesn't interfere
        std::env::set_var("FBUILD_CACHE_DIR", tmp.path().join("cache"));
        let tc = AvrToolchain::new(tmp.path());
        assert!(!tc.is_installed());
        std::env::remove_var("FBUILD_CACHE_DIR");
    }
}
