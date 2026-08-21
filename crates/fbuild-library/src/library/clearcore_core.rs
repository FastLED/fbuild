//! Teknic ClearCore Arduino core framework package.
//!
//! ClearCore is an ATSAME53-based board whose Arduino package carries its own
//! device headers, linker script, and precompiled ClearCore/LwIP libraries.

use std::path::{Path, PathBuf};

use crate::{CacheSubdir, Framework, PackageBase, PackageInfo};

const CLEARCORE_CORE_VERSION: &str = "1.7.4";
const CLEARCORE_CORE_URL: &str = "https://www.teknic.com/files/downloads/ClearCore-1.7.4.zip";
const CLEARCORE_CORE_CHECKSUM: &str =
    "87542411133e8b1b0bb88d12a5df6601c8054b61e213e358fa95bb08e8632270";

/// Official Teknic ClearCore Arduino package manager.
pub struct ClearCoreCores {
    base: PackageBase,
}

impl ClearCoreCores {
    pub fn new(project_dir: &Path) -> Self {
        Self {
            base: PackageBase::new(
                "clearcore-core",
                CLEARCORE_CORE_VERSION,
                CLEARCORE_CORE_URL,
                CLEARCORE_CORE_URL,
                Some(CLEARCORE_CORE_CHECKSUM),
                CacheSubdir::Platforms,
                project_dir,
            ),
        }
    }

    /// Construct with a consumer-supplied `platform_packages` override.
    pub fn with_override(project_dir: &Path, ovr: fbuild_config::PackageOverride) -> Self {
        Self {
            base: PackageBase::new(
                "clearcore-core",
                CLEARCORE_CORE_VERSION,
                CLEARCORE_CORE_URL,
                CLEARCORE_CORE_URL,
                Some(CLEARCORE_CORE_CHECKSUM),
                CacheSubdir::Platforms,
                project_dir,
            )
            .with_override(ovr),
        }
    }

    #[cfg(test)]
    fn with_cache_root(project_dir: &Path, cache_root: &Path) -> Self {
        Self {
            base: PackageBase::with_cache_root(
                "clearcore-core",
                CLEARCORE_CORE_VERSION,
                CLEARCORE_CORE_URL,
                CLEARCORE_CORE_URL,
                Some(CLEARCORE_CORE_CHECKSUM),
                CacheSubdir::Platforms,
                project_dir,
                cache_root,
            ),
        }
    }

    fn resolved_dir(&self) -> PathBuf {
        find_core_root(&self.base.install_path())
    }

    fn validate(install_dir: &Path) -> fbuild_core::Result<()> {
        let root = find_core_root(install_dir);
        let required = [
            root.join("cores/arduino/Arduino.h"),
            root.join("variants/clearcore/linker_scripts/gcc/flash_with_bootloader.ld"),
            root.join("Teknic/libClearCore/Release/libClearCore.a"),
            root.join("Teknic/LwIP/Release/libLwIP.a"),
        ];
        if let Some(missing) = required.iter().find(|path| !path.exists()) {
            return Err(fbuild_core::FbuildError::PackageError(format!(
                "ClearCore core missing required file {} (in {})",
                missing.display(),
                root.display()
            )));
        }
        Ok(())
    }

    pub fn get_core_dir(&self, core_name: &str) -> PathBuf {
        let requested = self.get_cores_dir().join(core_name);
        if requested.is_dir() {
            requested
        } else {
            self.get_cores_dir().join("arduino")
        }
    }

    pub fn get_variant_dir(&self, variant_name: &str) -> PathBuf {
        self.get_variants_dir().join(variant_name)
    }

    pub fn get_linker_script(&self, variant_name: &str) -> PathBuf {
        self.get_variant_dir(variant_name)
            .join("linker_scripts")
            .join("gcc")
            .join("flash_with_bootloader.ld")
    }

    pub fn get_system_include_dirs(&self, core_dir: &Path, variant_dir: &Path) -> Vec<PathBuf> {
        let root = self.resolved_dir();
        vec![
            core_dir.to_path_buf(),
            core_dir.join("api"),
            variant_dir.to_path_buf(),
            variant_dir
                .join("Third Party")
                .join("SAME53")
                .join("CMSIS")
                .join("Device")
                .join("Include"),
            root.join("Teknic").join("libClearCore").join("inc"),
            root.join("Teknic")
                .join("LwIP")
                .join("LwIP")
                .join("src")
                .join("include"),
            root.join("Teknic")
                .join("LwIP")
                .join("LwIP")
                .join("port")
                .join("include"),
        ]
    }

    pub fn get_library_dirs(&self) -> Vec<PathBuf> {
        let root = self.resolved_dir();
        vec![
            root.join("Teknic").join("libClearCore").join("Release"),
            root.join("Teknic").join("LwIP").join("Release"),
        ]
    }
}

#[async_trait::async_trait]
impl crate::Package for ClearCoreCores {
    async fn ensure_installed(&self) -> fbuild_core::Result<PathBuf> {
        if self.is_installed() {
            return Ok(self.resolved_dir());
        }

        let install_path = self.base.staged_install(Self::validate).await?;
        Ok(find_core_root(&install_path))
    }

    fn is_installed(&self) -> bool {
        self.base.is_cached() && Self::validate(&self.base.install_path()).is_ok()
    }

    fn get_info(&self) -> PackageInfo {
        self.base.get_info()
    }
}

impl Framework for ClearCoreCores {
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

fn find_core_root(install_dir: &Path) -> PathBuf {
    if install_dir.join("cores").is_dir() {
        return install_dir.to_path_buf();
    }

    if let Ok(entries) = std::fs::read_dir(install_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && path.join("cores").is_dir() {
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
    fn finds_nested_vendor_archive_root() {
        let tmp = tempfile::TempDir::new().unwrap();
        let nested = tmp.path().join("ClearCore-1.7.4");
        std::fs::create_dir_all(nested.join("cores/arduino")).unwrap();
        assert_eq!(find_core_root(tmp.path()), nested);
    }

    #[test]
    fn clearcore_core_is_not_installed_without_vendor_payload() {
        let tmp = tempfile::TempDir::new().unwrap();
        let core = ClearCoreCores::with_cache_root(tmp.path(), &tmp.path().join("cache"));
        assert!(!core.is_installed());
    }
}
