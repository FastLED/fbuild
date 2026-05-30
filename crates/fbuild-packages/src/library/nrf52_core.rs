//! Adafruit nRF52 Arduino core framework package.
//!
//! Downloads and manages the Adafruit nRF52 Arduino core from PlatformIO's
//! registry (which includes submodules like TinyUSB pre-bundled).
//! Provides paths to: cores/nRF5, variants/, libraries/.

use std::path::{Path, PathBuf};

use crate::{CacheSubdir, Framework, PackageBase, PackageInfo};

const NRF52_CORE_VERSION: &str = "1.10601.0";
const NRF52_CORE_URL: &str = "https://dl.registry.platformio.org/download/platformio/tool/framework-arduinoadafruitnrf52/1.10601.0/framework-arduinoadafruitnrf52-1.10601.0.tar.gz";

/// Adafruit nRF52 Arduino core framework manager.
pub struct Nrf52Cores {
    base: PackageBase,
    install_dir: Option<PathBuf>,
}

impl Nrf52Cores {
    pub fn new(project_dir: &Path) -> Self {
        Self {
            base: PackageBase::new(
                "nrf52-core",
                NRF52_CORE_VERSION,
                NRF52_CORE_URL,
                NRF52_CORE_URL,
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
                "nrf52-core",
                NRF52_CORE_VERSION,
                NRF52_CORE_URL,
                NRF52_CORE_URL,
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

        let arduino_h = root.join("cores/nRF5/Arduino.h");
        if !arduino_h.exists() {
            return Err(fbuild_core::FbuildError::PackageError(format!(
                "nRF52 core missing cores/nRF5/Arduino.h (in {})",
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
    ///
    /// fbuild ships Adafruit's `framework-arduinoadafruitnrf52`. PIO's board
    /// JSON for some Nordic dev kits names the variant the way *another*
    /// nRF52 Arduino framework (sandeepmistry's `arduino-nRF5`) names its
    /// variant directory — but the Adafruit framework uses different names
    /// for the same hardware. Apply a small alias map before falling back to
    /// the literal name, so PIO-matching board JSONs still resolve.
    /// See FastLED/fbuild#321.
    pub fn get_variant_dir(&self, variant_name: &str) -> PathBuf {
        resolve_nrf52_variant_dir(&self.get_variants_dir(), variant_name)
    }

    /// Get the linker script for a given script name.
    ///
    /// Adafruit nRF52 linker scripts live in `cores/nRF5/linker/` (not in
    /// the variant directory). The script name comes from the board JSON
    /// `build.arduino.ldscript` field (e.g. `nrf52840_s140_v6.ld`).
    pub fn get_linker_script(&self, ldscript_name: &str) -> PathBuf {
        self.get_linker_dir().join(ldscript_name)
    }

    /// Get the linker scripts directory (`cores/nRF5/linker/`).
    ///
    /// This must be added to the linker's library search path (`-L`) so that
    /// `INCLUDE "nrf52_common.ld"` directives in the linker scripts resolve.
    pub fn get_linker_dir(&self) -> PathBuf {
        self.resolved_dir()
            .join("cores")
            .join("nRF5")
            .join("linker")
    }

    /// List all .c, .cpp, .cc, and .s source files in the core.
    pub fn get_core_sources(&self, core_name: &str) -> Vec<PathBuf> {
        let core_dir = self.get_core_dir(core_name);
        collect_sources(&core_dir)
    }
}

impl crate::Package for Nrf52Cores {
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
        root.join("cores").join("nRF5").join("Arduino.h").exists()
    }

    fn get_info(&self) -> PackageInfo {
        self.base.get_info()
    }
}

impl Framework for Nrf52Cores {
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

/// Map a PIO board-JSON variant name to the equivalent Adafruit variant
/// directory name when they differ. Returns the literal input when no alias
/// applies.
///
/// PCA10056 is Nordic's product code for the nRF52840-DK; Adafruit's
/// framework ships `variants/pca10056/` for that board. PIO's nordicnrf52
/// platform follows sandeepmistry's `arduino-nRF5` naming (`nRF52DK`) — when
/// the board JSON tracks PIO upstream, we need this alias to find the
/// matching directory in Adafruit's tree.
///
/// Keep this map small and only add entries for board JSONs that we ship.
fn nrf52_variant_alias(variant_name: &str) -> Option<&'static str> {
    match variant_name {
        // nRF52840-DK: PIO/sandeepmistry name -> Adafruit/PCA product-code name.
        "nRF52DK" => Some("pca10056"),
        _ => None,
    }
}

/// Resolve a variant directory under `variants_dir`, honoring the literal
/// name first, then any PIO->Adafruit alias. Returns the literal path when
/// neither exists so the eventual file-open error surfaces with a meaningful
/// directory name.
fn resolve_nrf52_variant_dir(variants_dir: &Path, variant_name: &str) -> PathBuf {
    let primary = variants_dir.join(variant_name);
    if primary.is_dir() {
        return primary;
    }
    if let Some(aliased) = nrf52_variant_alias(variant_name) {
        let candidate = variants_dir.join(aliased);
        if candidate.is_dir() {
            return candidate;
        }
    }
    primary
}

/// Find the actual core root inside an extracted archive.
///
/// GitHub archives extract as `Adafruit_nRF52_Arduino-1.6.1/` with the core inside.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Package;

    #[test]
    fn test_nrf52_cores_not_installed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let core = Nrf52Cores::with_cache_root(tmp.path(), &tmp.path().join("cache"));
        assert!(!core.is_installed());
    }

    #[test]
    fn test_find_core_root_direct() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("cores/nRF5")).unwrap();
        assert_eq!(find_core_root(tmp.path()), tmp.path().to_path_buf());
    }

    #[test]
    fn test_find_core_root_nested() {
        let tmp = tempfile::TempDir::new().unwrap();
        let nested = tmp.path().join("Adafruit_nRF52_Arduino-1.6.1");
        std::fs::create_dir_all(nested.join("cores/nRF5")).unwrap();
        assert_eq!(find_core_root(tmp.path()), nested);
    }

    #[test]
    fn test_get_linker_script() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("cores/nRF5/linker")).unwrap();
        let core = Nrf52Cores::new(tmp.path());
        let ld = core.get_linker_script("nrf52840_s140_v6.ld");
        assert!(ld.to_string_lossy().contains("nrf52840_s140_v6.ld"));
        assert!(ld.to_string_lossy().contains("linker"));
    }

    #[test]
    fn test_get_linker_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("cores/nRF5/linker")).unwrap();
        let core = Nrf52Cores::new(tmp.path());
        let dir = core.get_linker_dir();
        assert!(dir.ends_with("linker"));
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
        let result = Nrf52Cores::validate(tmp.path());
        assert!(result.is_err());
    }

    /// FastLED/fbuild#321: nrf52840_dk board JSON says `variant = "nRF52DK"`
    /// (the sandeepmistry/PIO name), but fbuild installs Adafruit's framework
    /// which uses `variants/pca10056/` for the same hardware. The resolver
    /// must accept the PIO name and resolve it to the Adafruit-equivalent
    /// directory.
    #[test]
    fn nrf52dk_variant_resolves_to_pca10056_when_pio_name_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let variants_dir = tmp.path().join("variants");
        std::fs::create_dir_all(variants_dir.join("pca10056")).unwrap();

        let resolved = resolve_nrf52_variant_dir(&variants_dir, "nRF52DK");
        assert_eq!(resolved, variants_dir.join("pca10056"));
    }

    /// Sanity: when the literal variant dir *does* exist (e.g. Adafruit's own
    /// `feather_nrf52840_sense`), honor it directly without alias lookup.
    #[test]
    fn variant_uses_literal_name_when_present() {
        let tmp = tempfile::TempDir::new().unwrap();
        let variants_dir = tmp.path().join("variants");
        std::fs::create_dir_all(variants_dir.join("feather_nrf52840_sense")).unwrap();

        let resolved = resolve_nrf52_variant_dir(&variants_dir, "feather_nrf52840_sense");
        assert_eq!(resolved, variants_dir.join("feather_nrf52840_sense"));
    }

    /// When neither the literal name nor any alias exists, return the literal
    /// so the eventual file-open error has a meaningful path.
    #[test]
    fn variant_returns_literal_when_no_match() {
        let tmp = tempfile::TempDir::new().unwrap();
        let variants_dir = tmp.path().join("variants");
        std::fs::create_dir_all(&variants_dir).unwrap();

        let resolved = resolve_nrf52_variant_dir(&variants_dir, "totally_unknown_board");
        assert_eq!(resolved, variants_dir.join("totally_unknown_board"));
    }

    /// An aliased name (nRF52DK) that exists literally on disk should still
    /// take the literal path — the alias is only a fallback.
    #[test]
    fn aliased_name_prefers_literal_when_present() {
        let tmp = tempfile::TempDir::new().unwrap();
        let variants_dir = tmp.path().join("variants");
        std::fs::create_dir_all(variants_dir.join("nRF52DK")).unwrap();
        std::fs::create_dir_all(variants_dir.join("pca10056")).unwrap();

        let resolved = resolve_nrf52_variant_dir(&variants_dir, "nRF52DK");
        assert_eq!(resolved, variants_dir.join("nRF52DK"));
    }
}
