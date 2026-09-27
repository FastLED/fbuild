//! ARM CMSIS framework package.
//!
//! Downloads and manages ARM CMSIS headers from PlatformIO's registry.
//! Supports both legacy `CMSIS/Include` and modern `CMSIS/Core/Include` layouts.

use std::path::{Path, PathBuf};

use crate::{CacheSubdir, PackageBase, PackageInfo};

const CMSIS_VERSION: &str = "2.50700.210515";
const CMSIS_URL: &str = "https://dl.registry.platformio.org/download/platformio/tool/framework-cmsis/2.50700.210515/framework-cmsis-2.50700.210515.tar.gz";
const CMSIS_CHECKSUM: &str = "c45aee42cad60ce1167b3ee15f36f624bb0d9878d831d3d4e32665c47d9635bb";

/// ARM CMSIS framework manager.
pub struct CmsisFramework {
    base: PackageBase,
}

impl CmsisFramework {
    pub fn new(project_dir: &Path) -> Self {
        Self {
            base: PackageBase::new(
                "cmsis-framework",
                CMSIS_VERSION,
                CMSIS_URL,
                CMSIS_URL,
                Some(CMSIS_CHECKSUM),
                CacheSubdir::Platforms,
                project_dir,
            ),
        }
    }

    /// Use the CMSIS headers declared by the selected PlatformIO manifest.
    pub fn with_override(project_dir: &Path, ovr: fbuild_config::PackageOverride) -> Self {
        let mut framework = Self::new(project_dir);
        framework.base = framework.base.with_override(ovr);
        framework
    }

    #[cfg(test)]
    fn with_cache_root(project_dir: &Path, cache_root: &Path) -> Self {
        Self {
            base: PackageBase::with_cache_root(
                "cmsis-framework",
                CMSIS_VERSION,
                CMSIS_URL,
                CMSIS_URL,
                Some(CMSIS_CHECKSUM),
                CacheSubdir::Platforms,
                project_dir,
                cache_root,
            ),
        }
    }

    /// Validate the extracted package has required structure.
    fn validate(install_dir: &Path) -> fbuild_core::Result<()> {
        let core_cm4 = Self::core_include_at(install_dir).join("core_cm4.h");
        if !core_cm4.exists() {
            return Err(fbuild_core::FbuildError::PackageError(format!(
                "CMSIS missing core_cm4.h in modern or legacy include layout (in {})",
                install_dir.display()
            )));
        }
        Ok(())
    }

    fn core_include_at(root: &Path) -> PathBuf {
        let modern = root.join("CMSIS/Core/Include");
        if modern.join("core_cm4.h").exists() {
            modern
        } else {
            let legacy = root.join("CMSIS/Include");
            if legacy.join("core_cm4.h").exists() {
                legacy
            } else {
                modern
            }
        }
    }

    fn is_legacy_layout(root: &Path) -> bool {
        root.join("CMSIS/Include/core_cm4.h").exists()
            && !root.join("CMSIS/Core/Include/core_cm4.h").exists()
    }

    /// Get the CMSIS Core include directory (contains core_cm4.h, etc.).
    pub fn get_core_include_dir(&self) -> PathBuf {
        Self::core_include_at(&self.base.install_path())
    }

    /// Get the CMSIS DSP include directory.
    pub fn get_dsp_include_dir(&self) -> PathBuf {
        let root = self.base.install_path();
        if Self::is_legacy_layout(&root) {
            root.join("CMSIS/Include")
        } else {
            root.join("CMSIS/DSP/Include")
        }
    }

    /// Get the GCC CMSIS-DSP library directory.
    pub fn get_gcc_library_dir(&self) -> PathBuf {
        let root = self.base.install_path();
        if Self::is_legacy_layout(&root) {
            root.join("CMSIS/Lib/GCC")
        } else {
            root.join("CMSIS/DSP/Lib/GCC")
        }
    }
}

#[async_trait::async_trait]
impl crate::Package for CmsisFramework {
    async fn ensure_installed(&self) -> fbuild_core::Result<PathBuf> {
        if self.is_installed() {
            return Ok(self.base.install_path());
        }

        self.base.staged_install(Self::validate).await?;
        Ok(self.base.install_path())
    }

    fn is_installed(&self) -> bool {
        if !self.base.is_cached() {
            return false;
        }
        Self::core_include_at(&self.base.install_path())
            .join("core_cm4.h")
            .exists()
    }

    fn get_info(&self) -> PackageInfo {
        self.base.get_info()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Package;

    #[test]
    fn test_cmsis_not_installed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cmsis = CmsisFramework::with_cache_root(tmp.path(), &tmp.path().join("cache"));
        assert!(!cmsis.is_installed());
    }

    #[test]
    fn test_validate_missing_core_cm4() {
        let tmp = tempfile::TempDir::new().unwrap();
        let result = CmsisFramework::validate(tmp.path());
        assert!(result.is_err());
    }

    #[test]
    fn gcc_library_dir_points_to_dsp_payload() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cmsis = CmsisFramework::with_cache_root(tmp.path(), &tmp.path().join("cache"));
        assert!(
            cmsis
                .get_gcc_library_dir()
                .ends_with(Path::new("CMSIS/DSP/Lib/GCC"))
        );
    }

    #[test]
    fn accepts_legacy_cmsis_registry_layout() {
        let tmp = tempfile::TempDir::new().unwrap();
        let include = tmp.path().join("CMSIS/Include");
        std::fs::create_dir_all(&include).unwrap();
        std::fs::write(include.join("core_cm4.h"), "").unwrap();
        CmsisFramework::validate(tmp.path()).unwrap();
        assert_eq!(CmsisFramework::core_include_at(tmp.path()), include);
        assert!(CmsisFramework::is_legacy_layout(tmp.path()));
    }
}
