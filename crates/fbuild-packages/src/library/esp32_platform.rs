//! ESP32 pioarduino platform package.
//!
//! Downloads `platform-espressif32.zip` from pioarduino, which contains
//! `platform.json` with metadata URLs for toolchains and frameworks.

use std::path::{Path, PathBuf};

use fbuild_core::{FbuildError, Result};

use crate::{CacheSubdir, PackageBase, PackageInfo};

/// Platform release URL (stable channel).
const PLATFORM_URL: &str = "https://github.com/pioarduino/platform-espressif32/releases/download/stable/platform-espressif32.zip";

/// Platform version label.
const PLATFORM_VERSION: &str = "stable";

/// ESP32 platform package from pioarduino.
///
/// Provides access to `platform.json` for resolving toolchain metadata URLs.
pub struct Esp32Platform {
    base: PackageBase,
    install_dir: Option<PathBuf>,
}

impl Esp32Platform {
    pub fn new(project_dir: &Path) -> Self {
        Self {
            base: PackageBase::new(
                "platform-espressif32",
                PLATFORM_VERSION,
                PLATFORM_URL,
                PLATFORM_URL,
                None, // No checksum for stable release (content changes)
                CacheSubdir::Platforms,
                project_dir,
            ),
            install_dir: None,
        }
    }

    /// Get the resolved install directory.
    fn resolved_dir(&self) -> PathBuf {
        self.install_dir
            .clone()
            .unwrap_or_else(|| find_platform_root(&self.base.install_path()))
    }

    /// Get the toolchain metadata URL from platform.json.
    ///
    /// For RISC-V MCUs, returns the URL for `toolchain-riscv32-esp`.
    /// For Xtensa MCUs, returns the URL for `toolchain-xtensa-esp-elf`.
    pub fn get_toolchain_metadata_url(&self, is_riscv: bool) -> Result<String> {
        let package_name = if is_riscv {
            "toolchain-riscv32-esp"
        } else {
            "toolchain-xtensa-esp-elf"
        };
        self.get_package_url(package_name)
    }

    /// Get a package URL from platform.json by package name.
    ///
    /// The `packages` section of platform.json maps package names to objects
    /// with a `version` field that contains the metadata URL.
    pub fn get_package_url(&self, package_name: &str) -> Result<String> {
        let platform_json_path = self.resolved_dir().join("platform.json");

        let content = std::fs::read_to_string(&platform_json_path).map_err(|e| {
            FbuildError::PackageError(format!(
                "failed to read platform.json at {}: {}",
                platform_json_path.display(),
                e
            ))
        })?;

        let data: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
            FbuildError::PackageError(format!("failed to parse platform.json: {}", e))
        })?;

        let url = data
            .get("packages")
            .and_then(|p| p.get(package_name))
            .and_then(|pkg| pkg.get("version"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                FbuildError::PackageError(format!(
                    "package '{}' not found in platform.json",
                    package_name
                ))
            })?;

        Ok(url.to_string())
    }

    /// Validate the platform installation.
    fn validate_install(install_dir: &Path) -> Result<()> {
        let root = find_platform_root(install_dir);
        let platform_json = root.join("platform.json");

        if !platform_json.exists() {
            return Err(FbuildError::PackageError(format!(
                "platform.json not found in {}",
                root.display()
            )));
        }

        let boards_dir = root.join("boards");
        if !boards_dir.exists() {
            return Err(FbuildError::PackageError(format!(
                "boards directory not found in {}",
                root.display()
            )));
        }

        Ok(())
    }
}

impl crate::Package for Esp32Platform {
    fn ensure_installed(&self) -> Result<PathBuf> {
        if self.is_installed() {
            return Ok(self.resolved_dir());
        }

        let rt = tokio::runtime::Handle::try_current().ok();
        let install_path = if let Some(handle) = rt {
            handle.block_on(self.base.staged_install(Self::validate_install))?
        } else {
            let rt = tokio::runtime::Runtime::new().map_err(|e| {
                FbuildError::PackageError(format!("failed to create tokio runtime: {}", e))
            })?;
            rt.block_on(self.base.staged_install(Self::validate_install))?
        };

        Ok(find_platform_root(&install_path))
    }

    fn is_installed(&self) -> bool {
        if !self.base.is_cached() {
            return false;
        }
        let root = find_platform_root(&self.base.install_path());
        root.join("platform.json").exists()
    }

    fn get_info(&self) -> PackageInfo {
        self.base.get_info()
    }
}

/// Find the actual platform root directory (handles nested extraction).
fn find_platform_root(install_dir: &Path) -> PathBuf {
    if install_dir.join("platform.json").exists() {
        return install_dir.to_path_buf();
    }

    // Check one level deep for platform.json or platform-* directories
    if let Ok(entries) = std::fs::read_dir(install_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && path.join("platform.json").exists() {
                return path;
            }
        }
    }

    install_dir.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Package;

    #[test]
    fn test_platform_url() {
        assert!(PLATFORM_URL.contains("pioarduino"));
        assert!(PLATFORM_URL.contains("platform-espressif32"));
    }

    #[test]
    fn test_find_platform_root_direct() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("platform.json"), "{}").unwrap();
        std::fs::create_dir_all(tmp.path().join("boards")).unwrap();
        assert_eq!(find_platform_root(tmp.path()), tmp.path().to_path_buf());
    }

    #[test]
    fn test_find_platform_root_nested() {
        let tmp = tempfile::TempDir::new().unwrap();
        let nested = tmp.path().join("platform-espressif32");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("platform.json"), "{}").unwrap();
        assert_eq!(find_platform_root(tmp.path()), nested);
    }

    #[test]
    fn test_esp32_platform_not_installed() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::env::set_var("FBUILD_CACHE_DIR", tmp.path().join("cache"));
        let platform = Esp32Platform::new(tmp.path());
        assert!(!platform.is_installed());
        std::env::remove_var("FBUILD_CACHE_DIR");
    }

    #[test]
    fn test_validate_install() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("platform.json"), "{}").unwrap();
        std::fs::create_dir_all(tmp.path().join("boards")).unwrap();
        assert!(Esp32Platform::validate_install(tmp.path()).is_ok());
    }

    #[test]
    fn test_validate_install_missing_json() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(Esp32Platform::validate_install(tmp.path()).is_err());
    }
}
