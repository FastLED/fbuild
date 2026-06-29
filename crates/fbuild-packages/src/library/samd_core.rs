//! Adafruit SAMD core (ArduinoCore-samd) framework package.
//!
//! Downloads and manages the Adafruit SAMD core for SAMD21/SAMD51 boards from GitHub.
//! Provides paths to: `cores/arduino`, `variants/<board>`, and `libraries/`.

use std::path::{Path, PathBuf};

use crate::{CacheSubdir, Framework, PackageBase, PackageInfo};

const SAMD_CORE_VERSION: &str = "1.7.16";
const SAMD_CORE_URL: &str =
    "https://github.com/adafruit/ArduinoCore-samd/archive/refs/tags/1.7.16.tar.gz";

/// Adafruit SAMD core framework manager.
pub struct SamdCores {
    base: PackageBase,
    install_dir: Option<PathBuf>,
}

impl SamdCores {
    pub fn new(project_dir: &Path) -> Self {
        Self {
            base: PackageBase::new(
                "samd-core",
                SAMD_CORE_VERSION,
                SAMD_CORE_URL,
                SAMD_CORE_URL,
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
                "samd-core",
                SAMD_CORE_VERSION,
                SAMD_CORE_URL,
                SAMD_CORE_URL,
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

        let arduino_h = root.join("cores/arduino/Arduino.h");
        if !arduino_h.exists() {
            return Err(fbuild_core::FbuildError::PackageError(format!(
                "SAMD core missing cores/arduino/Arduino.h (in {})",
                root.display()
            )));
        }

        Ok(())
    }

    /// Get the core source directory for a specific core name.
    ///
    /// PlatformIO's `build.core` field is a vendor-branding label more than
    /// a real subdirectory name — Adafruit's ArduinoCore-samd (the SAMD
    /// framework we install) ships only `cores/arduino/`, but every Adafruit
    /// SAMD board declares `build.core = "adafruit"` so the literal
    /// `cores/adafruit/` lookup misses. Mirror PlatformIO's atmelsam builder
    /// fallback: if `cores/<core_name>/` doesn't exist on disk, fall back to
    /// `cores/arduino/` (the canonical name for Arduino-compatible cores).
    /// See FastLED/fbuild#319.
    pub fn get_core_dir(&self, core_name: &str) -> PathBuf {
        resolve_core_dir_with_arduino_fallback(&self.get_cores_dir(), core_name)
    }

    /// Get the variant directory for a specific variant name.
    pub fn get_variant_dir(&self, variant_name: &str) -> PathBuf {
        self.get_variants_dir().join(variant_name)
    }

    /// Get the linker script for a SAMD variant.
    ///
    /// SAMD variants store linker scripts in
    /// `variants/<variant>/linker_scripts/gcc/flash_with_bootloader.ld`.
    pub fn get_linker_script(&self, variant_name: &str) -> PathBuf {
        self.get_variant_dir(variant_name)
            .join("linker_scripts")
            .join("gcc")
            .join("flash_with_bootloader.ld")
    }

    /// List all .c, .cpp, .cc, and .s source files in the core.
    pub fn get_core_sources(&self, core_name: &str) -> Vec<PathBuf> {
        let core_dir = self.get_core_dir(core_name);
        collect_sources(&core_dir)
    }
}

#[async_trait::async_trait]
impl crate::Package for SamdCores {
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
            .join("Arduino.h")
            .exists()
    }

    fn get_info(&self) -> PackageInfo {
        self.base.get_info()
    }
}

impl Framework for SamdCores {
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
/// GitHub archives extract as `ArduinoCore-samd-1.7.16/` with the core inside.
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

/// Collect .c, .cpp, .cc, and .s source files from a directory (non-recursive).
fn collect_sources(dir: &Path) -> Vec<PathBuf> {
    let mut sources = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                let ext = path
                    .extension()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_lowercase();
                if matches!(ext.as_str(), "c" | "cpp" | "cc" | "s") {
                    sources.push(path);
                }
            }
        }
    }
    sources.sort();
    sources
}

/// Resolve a core source dir under `cores_dir`, falling back to
/// `cores/arduino/` when the literal `<core_name>` subdirectory is missing.
///
/// Adafruit (and some other vendor) Arduino-compatible cores set
/// `build.core = "<vendor>"` in their board JSON for branding even though
/// the actual sources live in `cores/arduino/`. PlatformIO's atmelsam
/// builder handles this transparently; fbuild needs to do the same.
fn resolve_core_dir_with_arduino_fallback(cores_dir: &Path, core_name: &str) -> PathBuf {
    let primary = cores_dir.join(core_name);
    if primary.is_dir() {
        return primary;
    }
    let fallback = cores_dir.join("arduino");
    if fallback.is_dir() {
        return fallback;
    }
    primary
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Package;

    #[test]
    fn test_samd_cores_not_installed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let core = SamdCores::with_cache_root(tmp.path(), &tmp.path().join("cache"));
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
        let nested = tmp.path().join("ArduinoCore-samd-1.7.16");
        std::fs::create_dir_all(nested.join("cores/arduino")).unwrap();
        assert_eq!(find_core_root(tmp.path()), nested);
    }

    #[test]
    fn test_get_linker_script() {
        let tmp = tempfile::TempDir::new().unwrap();
        let core = SamdCores::new(tmp.path());
        let script = core.get_linker_script("feather_m0");
        assert!(script
            .to_string_lossy()
            .contains("flash_with_bootloader.ld"));
        assert!(script.to_string_lossy().contains("linker_scripts"));
    }

    #[test]
    fn test_collect_sources() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("main.cpp"), "").unwrap();
        std::fs::write(tmp.path().join("wiring.c"), "").unwrap();
        std::fs::write(tmp.path().join("startup.S"), "").unwrap();
        std::fs::write(tmp.path().join("header.h"), "").unwrap();
        let sources = collect_sources(tmp.path());
        assert_eq!(sources.len(), 3);
    }

    #[test]
    fn test_validate_missing_arduino_h() {
        let tmp = tempfile::TempDir::new().unwrap();
        let result = SamdCores::validate(tmp.path());
        assert!(result.is_err());
    }

    /// FastLED/fbuild#319: Adafruit SAMD boards declare `build.core = "adafruit"`
    /// (a vendor brand label) but the framework only ships `cores/arduino/`.
    /// The resolver must fall back to `cores/arduino/` when the literal
    /// `cores/<core_name>/` doesn't exist on disk, mirroring PIO's atmelsam
    /// builder behavior.
    #[test]
    fn resolve_falls_back_to_arduino_when_named_dir_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cores_dir = tmp.path().join("cores");
        std::fs::create_dir_all(cores_dir.join("arduino")).unwrap();
        // No `cores/adafruit/` exists.

        let resolved = resolve_core_dir_with_arduino_fallback(&cores_dir, "adafruit");
        assert_eq!(resolved, cores_dir.join("arduino"));
    }

    /// Sanity: when the named core dir *does* exist, the fallback must not
    /// kick in — honor the literal name.
    #[test]
    fn resolve_uses_literal_name_when_present() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cores_dir = tmp.path().join("cores");
        std::fs::create_dir_all(cores_dir.join("custom_vendor")).unwrap();
        std::fs::create_dir_all(cores_dir.join("arduino")).unwrap();

        let resolved = resolve_core_dir_with_arduino_fallback(&cores_dir, "custom_vendor");
        assert_eq!(resolved, cores_dir.join("custom_vendor"));
    }

    /// When neither the named dir nor `cores/arduino/` exists, return the
    /// literal `cores/<name>/` so the eventual file-open error surfaces with
    /// a meaningful path (don't synthesize a nonexistent fallback).
    #[test]
    fn resolve_returns_literal_when_no_fallback_available() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cores_dir = tmp.path().join("cores");
        // No cores subdirs at all.

        let resolved = resolve_core_dir_with_arduino_fallback(&cores_dir, "vendor");
        assert_eq!(resolved, cores_dir.join("vendor"));
    }
}
