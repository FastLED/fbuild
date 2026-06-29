//! Apollo3 (SparkFun Arduino Apollo3) framework package.
//!
//! Downloads and manages the Arduino Apollo3 core from GitHub.
//! Provides paths to: cores/arduino, variants/, libraries/, tools/.
//! The Apollo3 core uses mbed-os with pre-built libraries and response files
//! in each variant directory.

use std::path::{Path, PathBuf};

use crate::{CacheSubdir, Framework, PackageBase, PackageInfo};

const APOLLO3_CORE_VERSION: &str = "2.2.1";
/// Use the Arduino board manager package which includes all submodules pre-packaged
/// (mbed-os, mbed-bridge, SVL uploader with linker scripts).
const APOLLO3_CORE_URL: &str =
    "https://github.com/sparkfun/Arduino_Apollo3/releases/download/v2.2.1/Arduino_Apollo3.tar.gz";

/// Apollo3 (SparkFun Arduino) core framework manager.
pub struct Apollo3Cores {
    base: PackageBase,
    install_dir: Option<PathBuf>,
}

impl Apollo3Cores {
    pub fn new(project_dir: &Path) -> Self {
        Self {
            base: PackageBase::new(
                "apollo3-core",
                APOLLO3_CORE_VERSION,
                APOLLO3_CORE_URL,
                APOLLO3_CORE_URL,
                None,
                CacheSubdir::Platforms,
                project_dir,
            ),
            install_dir: None,
        }
    }

    #[cfg(test)]
    fn with_cache_root(project_dir: &Path, cache_root: &Path) -> Self {
        Self {
            base: PackageBase::with_cache_root(
                "apollo3-core",
                APOLLO3_CORE_VERSION,
                APOLLO3_CORE_URL,
                APOLLO3_CORE_URL,
                None,
                CacheSubdir::Platforms,
                project_dir,
                cache_root,
            ),
            install_dir: None,
        }
    }

    /// Get the resolved root directory of the core.
    fn resolved_dir(&self) -> PathBuf {
        self.install_dir
            .clone()
            .unwrap_or_else(|| find_core_root(&self.base.install_path()))
    }

    /// Validate the extracted core has required structure.
    fn validate(install_dir: &Path) -> fbuild_core::Result<()> {
        let root = find_core_root(install_dir);

        // Arduino.h lives inside the mbed-bridge subdir in the Apollo3 core
        let arduino_h = root.join("cores/arduino/mbed-bridge/Arduino.h");
        if !arduino_h.exists() {
            return Err(fbuild_core::FbuildError::PackageError(format!(
                "Apollo3 core missing cores/arduino/mbed-bridge/Arduino.h (in {})",
                root.display()
            )));
        }

        Ok(())
    }

    /// Get the core source directory for a specific core name.
    pub fn get_core_dir(&self, core_name: &str) -> PathBuf {
        self.get_cores_dir().join(core_name)
    }

    /// Get the variant directory for a specific variant name.
    pub fn get_variant_dir(&self, variant_name: &str) -> PathBuf {
        self.get_variants_dir().join(variant_name)
    }

    /// Get the linker script for SVL (SparkFun Variable Loader).
    /// The linker script is at tools/uploaders/svl/0x10000.ld.
    pub fn get_linker_script(&self) -> PathBuf {
        self.resolved_dir()
            .join("tools")
            .join("uploaders")
            .join("svl")
            .join("0x10000.ld")
    }

    /// Get the mbed directory for a given variant.
    pub fn get_mbed_dir(&self, variant_name: &str) -> PathBuf {
        self.get_variant_dir(variant_name).join("mbed")
    }

    /// Read a mbed response file (e.g. `.c-flags`, `.cxx-flags`, `.ld-flags`).
    /// Returns the file contents as a string, or empty string if not found.
    pub fn read_mbed_response_file(&self, variant_name: &str, filename: &str) -> String {
        let path = self.get_mbed_dir(variant_name).join(filename);
        std::fs::read_to_string(&path).unwrap_or_default()
    }

    /// Get the path to the pre-built libmbed-os.a for a variant.
    pub fn get_mbed_lib(&self, variant_name: &str) -> PathBuf {
        self.get_mbed_dir(variant_name).join("libmbed-os.a")
    }

    /// Get the path to mbed_config.h for a variant.
    pub fn get_mbed_config_h(&self, variant_name: &str) -> PathBuf {
        self.get_mbed_dir(variant_name).join("mbed_config.h")
    }
}

#[async_trait::async_trait]
impl crate::Package for Apollo3Cores {
    async fn ensure_installed(&self) -> fbuild_core::Result<PathBuf> {
        if self.is_installed() {
            return Ok(self.resolved_dir());
        }

        let install_path = self.base.staged_install(Self::validate).await?;
        Ok(find_core_root(&install_path))
    }

    fn is_installed(&self) -> bool {
        if !self.base.is_cached() {
            return false;
        }
        let root = find_core_root(&self.base.install_path());
        root.join("cores")
            .join("arduino")
            .join("mbed-bridge")
            .join("Arduino.h")
            .exists()
    }

    fn get_info(&self) -> PackageInfo {
        self.base.get_info()
    }
}

impl Framework for Apollo3Cores {
    fn get_cores_dir(&self) -> PathBuf {
        self.resolved_dir().join("cores")
    }

    fn get_variants_dir(&self) -> PathBuf {
        self.resolved_dir().join("variants")
    }

    fn get_libraries_dir(&self) -> PathBuf {
        self.resolved_dir().join("libraries")
    }
}

/// Find the actual core root inside an extracted archive.
///
/// GitHub archives extract as `Arduino_Apollo3-2.2.1/` with the core inside.
fn find_core_root(install_dir: &Path) -> PathBuf {
    if install_dir.join("cores").exists() {
        return install_dir.to_path_buf();
    }

    if let Ok(entries) = std::fs::read_dir(install_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && path.join("cores").exists() {
                return path;
            }
        }
    }

    install_dir.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_apollo3_cores_not_installed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let core = Apollo3Cores::with_cache_root(tmp.path(), &tmp.path().join("cache"));
        assert!(!core.is_installed());
    }

    #[test]
    fn test_find_core_root_direct() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("cores/arduino")).unwrap();
        assert_eq!(find_core_root(tmp.path()), tmp.path().to_path_buf());
    }

    #[test]
    fn test_find_core_root_nested() {
        let tmp = tempfile::TempDir::new().unwrap();
        let nested = tmp.path().join("Arduino_Apollo3-2.2.1");
        std::fs::create_dir_all(nested.join("cores/arduino")).unwrap();
        assert_eq!(find_core_root(tmp.path()), nested);
    }

    #[test]
    fn test_get_linker_script() {
        let tmp = tempfile::TempDir::new().unwrap();
        let core = Apollo3Cores::new(tmp.path());
        let script = core.get_linker_script();
        assert!(script.to_string_lossy().contains("0x10000.ld"));
    }

    #[test]
    fn test_get_mbed_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let core = Apollo3Cores::new(tmp.path());
        let mbed_dir = core.get_mbed_dir("SFE_ARTEMIS_ATP");
        assert!(mbed_dir.to_string_lossy().contains("SFE_ARTEMIS_ATP"));
        assert!(mbed_dir.to_string_lossy().contains("mbed"));
    }

    #[test]
    fn test_validate_missing_arduino_h() {
        let tmp = tempfile::TempDir::new().unwrap();
        let result = Apollo3Cores::validate(tmp.path());
        assert!(result.is_err());
    }
}
