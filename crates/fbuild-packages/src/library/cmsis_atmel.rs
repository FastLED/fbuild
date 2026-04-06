//! CMSIS-Atmel device header package.
//!
//! Downloads and manages CMSIS-Atmel from PlatformIO's registry.
//! Provides CMSIS/Device/ATMEL/ headers (sam.h, samd21.h, samd51.h, etc.)

use std::path::{Path, PathBuf};

use crate::{CacheSubdir, PackageBase, PackageInfo};

const CMSIS_ATMEL_VERSION: &str = "1.2.2";
const CMSIS_ATMEL_URL: &str = "https://dl.registry.platformio.org/download/platformio/tool/framework-cmsis-atmel/1.2.2/framework-cmsis-atmel-1.2.2.tar.gz";

/// CMSIS-Atmel device header package manager.
pub struct CmsisAtmel {
    base: PackageBase,
}

impl CmsisAtmel {
    pub fn new(project_dir: &Path) -> Self {
        Self {
            base: PackageBase::new(
                "cmsis-atmel",
                CMSIS_ATMEL_VERSION,
                CMSIS_ATMEL_URL,
                CMSIS_ATMEL_URL,
                None,
                CacheSubdir::Platforms,
                project_dir,
            ),
        }
    }

    #[cfg(test)]
    fn with_cache_root(project_dir: &Path, cache_root: &Path) -> Self {
        Self {
            base: PackageBase::with_cache_root(
                "cmsis-atmel",
                CMSIS_ATMEL_VERSION,
                CMSIS_ATMEL_URL,
                CMSIS_ATMEL_URL,
                None,
                CacheSubdir::Platforms,
                project_dir,
                cache_root,
            ),
        }
    }

    /// Validate the extracted package has required structure.
    fn validate(install_dir: &Path) -> fbuild_core::Result<()> {
        let sam_h = install_dir
            .join("CMSIS")
            .join("Device")
            .join("ATMEL")
            .join("sam.h");
        if !sam_h.exists() {
            // Also check nested directory (archive may extract with prefix)
            let found = find_device_root(install_dir)
                .join("CMSIS")
                .join("Device")
                .join("ATMEL")
                .join("sam.h")
                .exists();
            if !found {
                return Err(fbuild_core::FbuildError::PackageError(format!(
                    "CMSIS-Atmel missing CMSIS/Device/ATMEL/sam.h (in {})",
                    install_dir.display()
                )));
            }
        }
        Ok(())
    }

    /// Get the CMSIS/Device/ATMEL include directory (contains sam.h, samd21.h, etc.).
    pub fn get_device_include_dir(&self) -> PathBuf {
        let root = find_device_root(&self.base.install_path());
        root.join("CMSIS").join("Device").join("ATMEL")
    }
}

/// Find the package root, handling nested archive extraction.
fn find_device_root(install_dir: &Path) -> PathBuf {
    if install_dir.join("CMSIS").exists() {
        return install_dir.to_path_buf();
    }

    if let Ok(entries) = std::fs::read_dir(install_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && path.join("CMSIS").exists() {
                return path;
            }
        }
    }

    install_dir.to_path_buf()
}

impl crate::Package for CmsisAtmel {
    fn ensure_installed(&self) -> fbuild_core::Result<PathBuf> {
        if self.is_installed() {
            return Ok(find_device_root(&self.base.install_path()));
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

        Ok(find_device_root(&install_path))
    }

    fn is_installed(&self) -> bool {
        if !self.base.is_cached() {
            return false;
        }
        let root = find_device_root(&self.base.install_path());
        root.join("CMSIS")
            .join("Device")
            .join("ATMEL")
            .join("sam.h")
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
    fn test_cmsis_atmel_not_installed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let pkg = CmsisAtmel::with_cache_root(tmp.path(), &tmp.path().join("cache"));
        assert!(!pkg.is_installed());
    }

    #[test]
    fn test_validate_missing_sam_h() {
        let tmp = tempfile::TempDir::new().unwrap();
        let result = CmsisAtmel::validate(tmp.path());
        assert!(result.is_err());
    }
}
