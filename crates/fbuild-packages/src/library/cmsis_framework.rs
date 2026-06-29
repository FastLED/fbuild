//! ARM CMSIS framework package.
//!
//! Downloads and manages ARM CMSIS 5.7.0 headers from PlatformIO's registry.
//! Provides paths to: CMSIS/Core/Include (core_cm4.h, etc.), CMSIS/DSP/Include.

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
        let core_cm4 = install_dir
            .join("CMSIS")
            .join("Core")
            .join("Include")
            .join("core_cm4.h");
        if !core_cm4.exists() {
            return Err(fbuild_core::FbuildError::PackageError(format!(
                "CMSIS missing CMSIS/Core/Include/core_cm4.h (in {})",
                install_dir.display()
            )));
        }
        Ok(())
    }

    /// Get the CMSIS Core include directory (contains core_cm4.h, etc.).
    pub fn get_core_include_dir(&self) -> PathBuf {
        self.base
            .install_path()
            .join("CMSIS")
            .join("Core")
            .join("Include")
    }

    /// Get the CMSIS DSP include directory.
    pub fn get_dsp_include_dir(&self) -> PathBuf {
        self.base
            .install_path()
            .join("CMSIS")
            .join("DSP")
            .join("Include")
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
        self.base
            .install_path()
            .join("CMSIS")
            .join("Core")
            .join("Include")
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
}
