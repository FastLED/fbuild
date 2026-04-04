//! Adafruit nRF52 Arduino core framework package.
//!
//! Downloads and manages the Adafruit nRF52 Arduino core from GitHub.
//! Provides paths to: cores/nRF5, variants/, libraries/.

use std::path::{Path, PathBuf};

use crate::{CacheSubdir, Framework, PackageBase, PackageInfo};

const NRF52_CORE_VERSION: &str = "1.6.1";
const NRF52_CORE_URL: &str =
    "https://github.com/adafruit/Adafruit_nRF52_Arduino/archive/refs/tags/1.6.1.tar.gz";

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
    pub fn get_variant_dir(&self, variant_name: &str) -> PathBuf {
        self.get_variants_dir().join(variant_name)
    }

    /// Get the linker script for a variant.
    ///
    /// nRF52 linker scripts are typically in `variants/<variant>/linker/` or
    /// directly in the variant directory. This method searches for .ld files.
    pub fn get_linker_script(&self, variant_name: &str) -> PathBuf {
        let variant_dir = self.get_variant_dir(variant_name);

        // First check linker/ subdirectory
        let linker_dir = variant_dir.join("linker");
        if linker_dir.is_dir() {
            if let Some(ld) = find_ld_file(&linker_dir) {
                return ld;
            }
        }

        // Fall back to searching the variant directory itself
        if let Some(ld) = find_ld_file(&variant_dir) {
            return ld;
        }

        // Default fallback path
        variant_dir.join("linker_script.ld")
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
    fn test_get_linker_script_from_linker_subdir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let variant_dir = tmp.path().join("variants/feather_nrf52840_express");
        let linker_dir = variant_dir.join("linker");
        std::fs::create_dir_all(&linker_dir).unwrap();
        std::fs::write(linker_dir.join("nrf52840_s140_v7.ld"), "").unwrap();

        // Create a core that points at tmp as root
        let core = Nrf52Cores::new(tmp.path());
        // We test find_ld_file directly since the core paths differ
        let ld = find_ld_file(&linker_dir);
        assert!(ld.is_some());
        assert!(ld.unwrap().to_string_lossy().contains(".ld"));

        // Suppress unused variable warning
        let _ = core;
    }

    #[test]
    fn test_find_ld_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("script.ld"), "").unwrap();
        std::fs::write(tmp.path().join("other.txt"), "").unwrap();
        let ld = find_ld_file(tmp.path());
        assert!(ld.is_some());
        assert!(ld.unwrap().to_string_lossy().contains("script.ld"));
    }

    #[test]
    fn test_find_ld_file_empty() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ld = find_ld_file(tmp.path());
        assert!(ld.is_none());
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
}
