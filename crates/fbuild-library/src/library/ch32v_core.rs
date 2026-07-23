//! OpenWCH CH32V Arduino core framework package.
//!
//! Downloads and manages the OpenWCH Arduino core for CH32V MCUs from GitHub.
//! Provides paths to: cores/arduino, variants/, libraries/.

use std::path::{Path, PathBuf};

use crate::{CacheSubdir, Framework, PackageBase, PackageInfo};

const CH32V_CORE_VERSION: &str = "1.0.4+d767162.ch32l103";
const CH32V_CORE_URL: &str = "https://github.com/openwch/arduino_core_ch32/archive/d76716239cdf8a084a5045c3dfd3151b3f69eeec.tar.gz";

/// OpenWCH CH32V Arduino core framework manager.
pub struct Ch32vCores {
    base: PackageBase,
    install_dir: Option<PathBuf>,
}

impl Ch32vCores {
    pub fn new(project_dir: &Path) -> Self {
        Self {
            base: PackageBase::new(
                "ch32v-core",
                CH32V_CORE_VERSION,
                CH32V_CORE_URL,
                CH32V_CORE_URL,
                None,
                CacheSubdir::Platforms,
                project_dir,
            ),
            install_dir: None,
        }
    }

    /// Construct with a consumer-supplied override (parsed from the env's
    /// `platform_packages` line in `platformio.ini`). The default const-pinned
    /// URL / version / checksum are replaced; `cache_subdir` and `name` are
    /// preserved. See `PackageBase::with_override` and FastLED/fbuild#681.
    pub fn with_override(project_dir: &Path, ovr: fbuild_config::PackageOverride) -> Self {
        Self {
            base: PackageBase::new(
                "ch32v-core",
                CH32V_CORE_VERSION,
                CH32V_CORE_URL,
                CH32V_CORE_URL,
                None,
                CacheSubdir::Platforms,
                project_dir,
            )
            .with_override(ovr),
            install_dir: None,
        }
    }

    #[cfg(test)]
    fn with_cache_root(project_dir: &Path, cache_root: &Path) -> Self {
        Self {
            base: PackageBase::with_cache_root(
                "ch32v-core",
                CH32V_CORE_VERSION,
                CH32V_CORE_URL,
                CH32V_CORE_URL,
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

        let cores_dir = root.join("cores");
        if !cores_dir.exists() {
            return Err(fbuild_core::FbuildError::PackageError(format!(
                "CH32V core missing cores/ directory (in {})",
                root.display()
            )));
        }

        Ok(())
    }

    /// Get the core source directory for a specific core name.
    ///
    /// The board JSON `core` field (e.g. "openwch") comes from PlatformIO's
    /// board definition and may not match the actual directory name inside the
    /// core package (which is typically `cores/arduino/`).  When the named
    /// directory doesn't exist, fall back to the first subdirectory of `cores/`.
    pub fn get_core_dir(&self, core_name: &str) -> PathBuf {
        let named = self.get_cores_dir().join(core_name);
        if named.exists() {
            return named;
        }
        // Auto-detect: pick the first subdirectory in cores/
        let cores_dir = self.get_cores_dir();
        if let Ok(entries) = std::fs::read_dir(&cores_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    tracing::debug!(
                        "CH32V core dir '{}' not found, using '{}'",
                        core_name,
                        path.display()
                    );
                    return path;
                }
            }
        }
        named
    }

    /// Get the variant directory for a specific variant name.
    pub fn get_variant_dir(&self, variant_name: &str) -> PathBuf {
        self.get_variants_dir().join(variant_name)
    }
}

#[async_trait::async_trait]
impl crate::Package for Ch32vCores {
    async fn ensure_installed(&self) -> fbuild_core::Result<PathBuf> {
        if self.is_installed() {
            let root = self.resolved_dir();
            return Ok(root);
        }

        let install_path = self.base.staged_install(Self::validate).await?;

        let root = find_core_root(&install_path);
        Ok(root)
    }

    fn is_installed(&self) -> bool {
        if !self.base.is_cached() {
            return false;
        }
        let root = find_core_root(&self.base.install_path());
        root.join("cores").exists()
    }

    fn get_info(&self) -> PackageInfo {
        self.base.get_info()
    }
}

impl Framework for Ch32vCores {
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
/// GitHub archives extract as `arduino_core_ch32-1.0.4/` with the core inside.
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
    use crate::Package;

    #[test]
    fn test_ch32v_cores_not_installed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let core = Ch32vCores::with_cache_root(tmp.path(), &tmp.path().join("cache"));
        assert!(!core.is_installed());
    }

    #[test]
    fn test_find_core_root_direct() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("cores/openwch")).unwrap();
        assert_eq!(find_core_root(tmp.path()), tmp.path().to_path_buf());
    }

    #[test]
    fn test_find_core_root_nested() {
        let tmp = tempfile::TempDir::new().unwrap();
        let nested = tmp.path().join("arduino_core_ch32-1.0.4");
        std::fs::create_dir_all(nested.join("cores/openwch")).unwrap();
        assert_eq!(find_core_root(tmp.path()), nested);
    }

    #[test]
    fn test_validate_missing_cores() {
        let tmp = tempfile::TempDir::new().unwrap();
        let result = Ch32vCores::validate(tmp.path());
        assert!(result.is_err());
    }
}
