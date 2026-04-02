//! ATTinyCore Arduino framework package.
//!
//! Downloads and manages the ATTinyCore from GitHub (SpenceKonde/ATTinyCore).
//! Provides paths to: cores/tiny, variants/tinyX5, etc.

use std::path::{Path, PathBuf};

use crate::{CacheSubdir, Framework, PackageBase, PackageInfo};

const ATTINY_CORE_VERSION: &str = "1.5.2";
const ATTINY_CORE_URL: &str =
    "https://github.com/SpenceKonde/ATTinyCore/archive/refs/tags/v1.5.2.tar.gz";

/// ATTinyCore Arduino framework manager.
pub struct ATTinyCore {
    base: PackageBase,
    install_dir: Option<PathBuf>,
}

impl ATTinyCore {
    pub fn new(project_dir: &Path) -> Self {
        Self {
            base: PackageBase::new(
                "attiny-core",
                ATTINY_CORE_VERSION,
                ATTINY_CORE_URL,
                ATTINY_CORE_URL,
                None, // No checksum verification for now
                CacheSubdir::Platforms,
                project_dir,
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

        let required = ["cores/tiny", "variants"];

        for path in &required {
            if !root.join(path).exists() {
                return Err(fbuild_core::FbuildError::PackageError(format!(
                    "ATTinyCore missing required path: {} (in {})",
                    path,
                    root.display()
                )));
            }
        }

        // Check for key source files
        let arduino_h = root.join("cores/tiny/Arduino.h");
        if !arduino_h.exists() {
            return Err(fbuild_core::FbuildError::PackageError(
                "ATTinyCore missing cores/tiny/Arduino.h".to_string(),
            ));
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
}

impl crate::Package for ATTinyCore {
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

        Ok(find_core_root(&install_path))
    }

    fn is_installed(&self) -> bool {
        if !self.base.is_cached() {
            return false;
        }
        let root = find_core_root(&self.base.install_path());
        root.join("cores").join("tiny").exists()
    }

    fn get_info(&self) -> PackageInfo {
        self.base.get_info()
    }
}

impl Framework for ATTinyCore {
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
/// GitHub archives extract as `ATTinyCore-1.5.2/` with the core inside.
fn find_core_root(install_dir: &Path) -> PathBuf {
    if install_dir.join("cores").exists() {
        return install_dir.to_path_buf();
    }

    // Check one level deep
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
    fn test_attiny_core_not_installed() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::env::set_var("FBUILD_CACHE_DIR", tmp.path().join("cache"));
        let core = ATTinyCore::new(tmp.path());
        assert!(!core.is_installed());
        std::env::remove_var("FBUILD_CACHE_DIR");
    }

    #[test]
    fn test_find_core_root_direct() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("cores/tiny")).unwrap();
        assert_eq!(find_core_root(tmp.path()), tmp.path().to_path_buf());
    }

    #[test]
    fn test_find_core_root_nested() {
        let tmp = tempfile::TempDir::new().unwrap();
        let nested = tmp.path().join("ATTinyCore-1.5.2");
        std::fs::create_dir_all(nested.join("cores/tiny")).unwrap();
        assert_eq!(find_core_root(tmp.path()), nested);
    }
}
