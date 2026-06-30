//! OpenWCH CH32V Arduino core framework package.
//!
//! Downloads and manages the OpenWCH Arduino core for CH32V MCUs from GitHub.
//! Provides paths to: cores/arduino, variants/, libraries/.

use std::path::{Path, PathBuf};

use crate::{CacheSubdir, Framework, PackageBase, PackageInfo};

const CH32V_CORE_VERSION: &str = "1.0.4";
const CH32V_CORE_URL: &str =
    "https://github.com/openwch/arduino_core_ch32/archive/refs/tags/1.0.4.tar.gz";

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

    /// Apply compatibility patches for known upstream core issues.
    fn patch_compatibility(root: &Path) -> fbuild_core::Result<()> {
        patch_backup_header(root)
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

    /// Get the linker script for a variant.
    ///
    /// CH32V variants have .ld linker scripts in the variant directory.
    pub fn get_linker_script(&self, variant_name: &str) -> PathBuf {
        let variant_dir = self.get_variant_dir(variant_name);

        // Search for .ld files in the variant directory
        if let Some(ld) = find_ld_file(&variant_dir) {
            return ld;
        }

        // Default fallback
        variant_dir.join("link.ld")
    }
}

#[async_trait::async_trait]
impl crate::Package for Ch32vCores {
    async fn ensure_installed(&self) -> fbuild_core::Result<PathBuf> {
        if self.is_installed() {
            let root = self.resolved_dir();
            Self::patch_compatibility(&root)?;
            return Ok(root);
        }

        let install_path = self.base.staged_install(Self::validate).await?;

        let root = find_core_root(&install_path);
        Self::patch_compatibility(&root)?;
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

/// Find the first .ld file in a directory.
fn find_ld_file(dir: &Path) -> Option<PathBuf> {
    let mut ld_files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(ext) = path.extension() {
                    if ext == "ld" {
                        ld_files.push(path);
                    }
                }
            }
        }
    }
    ld_files.sort();
    ld_files.into_iter().next()
}

fn patch_backup_header(root: &Path) -> fbuild_core::Result<()> {
    let backup_h = root
        .join("cores")
        .join("arduino")
        .join("ch32")
        .join("backup.h");
    if !backup_h.exists() {
        return Ok(());
    }

    let content = std::fs::read_to_string(&backup_h).map_err(|e| {
        fbuild_core::FbuildError::PackageError(format!(
            "failed to read CH32V backup header {}: {}",
            backup_h.display(),
            e
        ))
    })?;

    let patched = content.replace("#ifndef CH32V00x", "#ifdef RCC_APB1Periph_BKP");
    if patched != content {
        std::fs::write(&backup_h, patched).map_err(|e| {
            fbuild_core::FbuildError::PackageError(format!(
                "failed to patch CH32V backup header {}: {}",
                backup_h.display(),
                e
            ))
        })?;
        tracing::debug!("patched {}", backup_h.display());
    }

    Ok(())
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
    fn test_get_linker_script_with_ld_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let variant_dir = tmp.path().join("variants/CH32V00x/CH32V003F4");
        std::fs::create_dir_all(&variant_dir).unwrap();
        std::fs::write(variant_dir.join("link.ld"), "").unwrap();

        let ld = find_ld_file(&variant_dir);
        assert!(ld.is_some());
        assert!(ld.unwrap().to_string_lossy().contains("link.ld"));
    }

    #[test]
    fn test_get_linker_script_fallback() {
        let tmp = tempfile::TempDir::new().unwrap();
        let core = Ch32vCores::new(tmp.path());
        let script = core.get_linker_script("CH32V00x/CH32V003F4");
        assert!(script.to_string_lossy().contains("link.ld"));
    }

    #[test]
    fn test_validate_missing_cores() {
        let tmp = tempfile::TempDir::new().unwrap();
        let result = Ch32vCores::validate(tmp.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_patch_backup_header_replaces_old_guard() {
        let tmp = tempfile::TempDir::new().unwrap();
        let backup_h = tmp.path().join("cores/arduino/ch32/backup.h");
        std::fs::create_dir_all(backup_h.parent().unwrap()).unwrap();
        std::fs::write(
            &backup_h,
            "static inline void resetBackupDomain(void)\n{\n#ifndef CH32V00x\n  RCC_BackupResetCmd(ENABLE);\n  RCC_BackupResetCmd(DISABLE);\n#endif\n}\n",
        )
        .unwrap();

        patch_backup_header(tmp.path()).unwrap();

        let patched = std::fs::read_to_string(&backup_h).unwrap();
        assert!(patched.contains("#ifdef RCC_APB1Periph_BKP"));
        assert!(!patched.contains("#ifndef CH32V00x"));
    }

    #[test]
    fn test_patch_backup_header_is_noop_when_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        patch_backup_header(tmp.path()).unwrap();
    }
}
