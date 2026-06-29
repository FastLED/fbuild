//! ESP32 Arduino framework package.
//!
//! Downloads and manages the Arduino-ESP32 core + ESP-IDF precompiled libraries.
//! This combines what PlatformIO splits into two packages:
//! - `framework-arduinoespressif32`: Arduino core, variants, libraries
//! - `framework-arduinoespressif32-libs`: ESP-IDF SDK includes + precompiled `.a` libs
//!
//! Key methods provide paths to:
//! - Core sources: `cores/esp32/`
//! - Board variants: `variants/{mcu}/`
//! - SDK include dirs: `tools/sdk/{mcu}/include/` (305+ paths)
//! - SDK precompiled libs: `tools/sdk/{mcu}/lib/` (100+ .a files)
//! - Linker scripts: `tools/sdk/{mcu}/ld/`
//! - Bootloader/partitions: `tools/sdk/{mcu}/bin/`

use std::path::{Path, PathBuf};

use crate::{CacheSubdir, Framework, PackageBase, PackageInfo};

mod fs_utils;
mod libs;
mod parsing;
mod paths;
mod sdk_paths;

#[cfg(test)]
mod tests;

use fs_utils::find_framework_root;
use parsing::extract_framework_version;

const ESP32_FRAMEWORK_VERSION: &str = "3.1.1";
const ESP32_FRAMEWORK_URL: &str =
    "https://github.com/pioarduino/arduino-esp32/releases/download/3.1.1/framework-arduinoespressif32-3.1.1.tar.gz";

/// ESP32 Arduino framework manager.
pub struct Esp32Framework {
    pub(crate) base: PackageBase,
    pub(crate) install_dir: Option<PathBuf>,
}

impl Esp32Framework {
    /// Create with hardcoded URL (legacy, for tests).
    pub fn new(project_dir: &Path, _mcu: &str) -> Self {
        Self {
            base: PackageBase::new(
                "esp32-arduino",
                ESP32_FRAMEWORK_VERSION,
                ESP32_FRAMEWORK_URL,
                ESP32_FRAMEWORK_URL,
                None,
                CacheSubdir::Platforms,
                project_dir,
            ),
            install_dir: None,
        }
    }

    #[cfg(test)]
    fn with_cache_root(project_dir: &Path, cache_root: &Path, _mcu: &str) -> Self {
        Self {
            base: PackageBase::with_cache_root(
                "esp32-arduino",
                ESP32_FRAMEWORK_VERSION,
                ESP32_FRAMEWORK_URL,
                ESP32_FRAMEWORK_URL,
                None,
                CacheSubdir::Platforms,
                project_dir,
                cache_root,
            ),
            install_dir: None,
        }
    }

    /// Create from a resolved URL (from platform.json).
    ///
    /// The orchestrator reads `platform.json` → `packages.framework-arduinoespressif32.version`
    /// to get the correct download URL (e.g. espressif/arduino-esp32 release).
    pub fn from_url(project_dir: &Path, url: &str) -> Self {
        // Extract version from URL (e.g., "3.3.7" from ".../3.3.7/esp32-core-3.3.7.tar.xz")
        let version = extract_framework_version(url);

        Self {
            base: PackageBase::new(
                "esp32-arduino",
                &version,
                url,
                "framework-arduinoespressif32",
                None,
                CacheSubdir::Platforms,
                project_dir,
            ),
            install_dir: None,
        }
    }

    /// Get the resolved root directory of the framework.
    pub(crate) fn resolved_dir(&self) -> PathBuf {
        self.install_dir
            .clone()
            .unwrap_or_else(|| find_framework_root(&self.base.install_path()))
    }

    /// Validate the extracted framework has required structure.
    fn validate(install_dir: &Path) -> fbuild_core::Result<()> {
        let root = find_framework_root(install_dir);

        let cores_dir = root.join("cores").join("esp32");
        if !cores_dir.exists() {
            return Err(fbuild_core::FbuildError::PackageError(format!(
                "ESP32 framework missing cores/esp32/ directory (in {})",
                root.display()
            )));
        }

        let arduino_h = cores_dir.join("Arduino.h");
        if !arduino_h.exists() {
            return Err(fbuild_core::FbuildError::PackageError(
                "ESP32 framework missing cores/esp32/Arduino.h".to_string(),
            ));
        }

        Ok(())
    }
}

#[async_trait::async_trait]
impl crate::Package for Esp32Framework {
    async fn ensure_installed(&self) -> fbuild_core::Result<PathBuf> {
        if self.is_installed() {
            return Ok(self.resolved_dir());
        }

        let install_path = self.base.staged_install(Self::validate).await?;
        Ok(find_framework_root(&install_path))
    }

    fn is_installed(&self) -> bool {
        if !self.base.is_cached() {
            return false;
        }
        let root = find_framework_root(&self.base.install_path());
        root.join("cores").join("esp32").join("Arduino.h").exists()
    }

    fn get_info(&self) -> PackageInfo {
        self.base.get_info()
    }
}

impl Framework for Esp32Framework {
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
