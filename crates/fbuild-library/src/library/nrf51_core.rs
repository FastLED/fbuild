//! sandeepmistry arduino-nRF5 core framework package (nRF51 boards).
//!
//! PlatformIO's `nordicnrf51` platform builds Arduino sketches against
//! `framework-arduinonordicnrf5` (sandeepmistry/arduino-nRF5). Unlike the
//! Adafruit nRF52 BSP it has no RTOS, no TinyUSB and no SoftDevice by
//! default; the Nordic SDK subset lives under `cores/nRF5/SDK/components`.
//! The pinned version follows platform-nordicnrf51's `~1.700.0` requirement.

use std::path::{Path, PathBuf};

use fbuild_core::path::NormalizedPath;

use crate::{CacheSubdir, PackageBase, PackageInfo};

const NRF51_CORE_NAME: &str = "nrf51-core";
const NRF51_CORE_VERSION: &str = "1.700.240108";
const NRF51_CORE_URL: &str = "https://dl.registry.platformio.org/download/platformio/tool/framework-arduinonordicnrf5/1.700.240108/framework-arduinonordicnrf5-1.700.240108.tar.gz";
const NRF51_CORE_SHA256: &str = "b28f0441db0df340c917b277a0f95b5afa11004977711153df2302a9778215cd";

/// sandeepmistry arduino-nRF5 core framework manager.
pub struct Nrf51Cores {
    base: PackageBase,
}

impl Nrf51Cores {
    pub fn new(project_dir: &Path) -> Self {
        Self {
            base: Self::default_base(project_dir),
        }
    }

    /// Construct with a consumer-supplied `platform_packages` override for
    /// `framework-arduinonordicnrf5`. See `PackageBase::with_override` and
    /// FastLED/fbuild#681.
    pub fn with_override(project_dir: &Path, ovr: fbuild_config::PackageOverride) -> Self {
        Self {
            base: Self::default_base(project_dir).with_override(ovr),
        }
    }

    fn default_base(project_dir: &Path) -> PackageBase {
        PackageBase::new(
            NRF51_CORE_NAME,
            NRF51_CORE_VERSION,
            NRF51_CORE_URL,
            NRF51_CORE_URL,
            Some(NRF51_CORE_SHA256),
            CacheSubdir::Platforms,
            project_dir,
        )
    }

    #[cfg(test)]
    fn with_cache_root(project_dir: &Path, cache_root: &Path) -> Self {
        Self {
            base: PackageBase::with_cache_root(
                NRF51_CORE_NAME,
                NRF51_CORE_VERSION,
                NRF51_CORE_URL,
                NRF51_CORE_URL,
                Some(NRF51_CORE_SHA256),
                CacheSubdir::Platforms,
                project_dir,
                cache_root,
            ),
        }
    }

    fn resolved_dir(&self) -> NormalizedPath {
        find_core_root(&self.base.install_path())
    }

    fn validate(install_dir: &Path) -> fbuild_core::Result<()> {
        let root = find_core_root(install_dir);
        let required = [
            "cores/nRF5/Arduino.h",
            "cores/nRF5/SDK/components/toolchain/gcc/gcc_startup_nrf51.S",
        ];
        for rel in required {
            if !root.join(rel).is_file() {
                return Err(fbuild_core::FbuildError::PackageError(format!(
                    "arduino-nRF5 core missing {} (in {})",
                    rel,
                    root.display()
                )));
            }
        }
        Ok(())
    }

    /// `cores/<core_name>` (e.g. `cores/nRF5`).
    pub fn get_core_dir(&self, core_name: &str) -> NormalizedPath {
        self.resolved_dir().join("cores").join(core_name)
    }

    /// `variants/<variant_name>` (e.g. `variants/BBCmicrobit`).
    pub fn get_variant_dir(&self, variant_name: &str) -> NormalizedPath {
        self.resolved_dir().join("variants").join(variant_name)
    }

    /// `libraries/` (SPI, Wire, ...).
    pub fn get_libraries_dir(&self) -> NormalizedPath {
        self.resolved_dir().join("libraries")
    }

    /// `cores/<core>/SDK/components` — the bundled Nordic SDK subset.
    pub fn get_sdk_components_dir(&self, core_name: &str) -> NormalizedPath {
        self.get_core_dir(core_name).join("SDK").join("components")
    }

    /// GCC linker-script directory. Must be on the `-L` path so the board
    /// script's `INCLUDE "nrf51_common.ld"` resolves.
    pub fn get_linker_dir(&self, core_name: &str) -> NormalizedPath {
        self.get_sdk_components_dir(core_name)
            .join("toolchain")
            .join("gcc")
    }

    /// Board linker script (`build.arduino.ldscript`, e.g. `nrf51_xxaa.ld`).
    pub fn get_linker_script(&self, core_name: &str, ldscript_name: &str) -> NormalizedPath {
        self.get_linker_dir(core_name).join(ldscript_name)
    }
}

#[async_trait::async_trait]
impl crate::Package for Nrf51Cores {
    async fn ensure_installed(&self) -> fbuild_core::Result<PathBuf> {
        if self.is_installed() {
            return Ok(self.resolved_dir().into_path_buf());
        }
        let install_path = self.base.staged_install(Self::validate).await?;
        Ok(find_core_root(&install_path).into_path_buf())
    }

    fn is_installed(&self) -> bool {
        self.base.is_cached() && Self::validate(&self.base.install_path()).is_ok()
    }

    fn get_info(&self) -> PackageInfo {
        self.base.get_info()
    }
}

/// The registry tarball extracts with `cores/` at the root; tolerate a single
/// wrapping directory (GitHub archive form used by `platform_packages` URLs).
fn find_core_root(install_dir: &Path) -> NormalizedPath {
    if install_dir.join("cores").is_dir() {
        return NormalizedPath::new(install_dir);
    }
    if let Ok(entries) = std::fs::read_dir(install_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.join("cores").is_dir() {
                return NormalizedPath::new(path);
            }
        }
    }
    NormalizedPath::new(install_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Package;

    #[test]
    fn not_installed_in_empty_cache() {
        let tmp = tempfile::TempDir::new().unwrap();
        let core = Nrf51Cores::with_cache_root(tmp.path(), &tmp.path().join("cache"));
        assert!(!core.is_installed());
        assert_eq!(core.get_info().version, NRF51_CORE_VERSION);
    }

    #[test]
    fn find_core_root_accepts_flat_and_wrapped_layouts() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("cores/nRF5")).unwrap();
        assert_eq!(find_core_root(tmp.path()).as_path(), tmp.path());

        let wrapped = tempfile::TempDir::new().unwrap();
        let inner = wrapped.path().join("arduino-nRF5-master");
        std::fs::create_dir_all(inner.join("cores/nRF5")).unwrap();
        assert_eq!(find_core_root(wrapped.path()).as_path(), inner.as_path());
    }

    #[test]
    fn validate_requires_arduino_h_and_nrf51_startup() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("cores/nRF5")).unwrap();
        std::fs::write(tmp.path().join("cores/nRF5/Arduino.h"), "").unwrap();
        assert!(Nrf51Cores::validate(tmp.path()).is_err());

        let gcc = tmp.path().join("cores/nRF5/SDK/components/toolchain/gcc");
        std::fs::create_dir_all(&gcc).unwrap();
        std::fs::write(gcc.join("gcc_startup_nrf51.S"), "").unwrap();
        assert!(Nrf51Cores::validate(tmp.path()).is_ok());
    }

    #[test]
    fn linker_script_lives_in_sdk_gcc_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let core = Nrf51Cores::with_cache_root(tmp.path(), &tmp.path().join("cache"));
        let script = core.get_linker_script("nRF5", "nrf51_xxaa.ld");
        assert!(
            script
                .as_path()
                .ends_with("cores/nRF5/SDK/components/toolchain/gcc/nrf51_xxaa.ld")
        );
        assert_eq!(
            script.as_path().parent().unwrap(),
            core.get_linker_dir("nRF5").as_path()
        );
    }
}
