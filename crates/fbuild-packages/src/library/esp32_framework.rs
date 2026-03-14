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

const ESP32_FRAMEWORK_VERSION: &str = "3.1.1";
const ESP32_FRAMEWORK_URL: &str =
    "https://github.com/pioarduino/arduino-esp32/releases/download/3.1.1/framework-arduinoespressif32-3.1.1.tar.gz";

/// ESP32 Arduino framework manager.
pub struct Esp32Framework {
    base: PackageBase,
    install_dir: Option<PathBuf>,
}

impl Esp32Framework {
    pub fn new(project_dir: &Path, _mcu: &str) -> Self {
        Self {
            base: PackageBase::new(
                "esp32-arduino",
                ESP32_FRAMEWORK_VERSION,
                ESP32_FRAMEWORK_URL,
                ESP32_FRAMEWORK_URL,
                None, // TODO: add checksum after first verified download
                CacheSubdir::Platforms,
                project_dir,
            ),
            install_dir: None,
        }
    }

    /// Get the resolved root directory of the framework.
    fn resolved_dir(&self) -> PathBuf {
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

    /// Get the core source directory (e.g. `cores/esp32`).
    pub fn get_core_dir(&self, core_name: &str) -> PathBuf {
        self.resolved_dir().join("cores").join(core_name)
    }

    /// Get the variant directory for a board (e.g. `variants/esp32c6`).
    pub fn get_variant_dir(&self, variant_name: &str) -> PathBuf {
        self.resolved_dir().join("variants").join(variant_name)
    }

    /// Get SDK include directories for a given MCU.
    ///
    /// Reads the `flags/includes` file from the SDK directory, which lists
    /// all 305+ include paths. Falls back to scanning `include/` subdirectories.
    pub fn get_sdk_include_dirs(&self, mcu: &str) -> Vec<PathBuf> {
        let root = self.resolved_dir();
        let sdk_dir = root.join("tools").join("sdk").join(mcu);

        // Try reading the includes list file first (used by pioarduino)
        let includes_file = sdk_dir.join("flags").join("includes");
        if includes_file.exists() {
            if let Ok(content) = std::fs::read_to_string(&includes_file) {
                return content
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .map(|line| {
                        let path = line.trim().trim_start_matches("-I");
                        // Resolve relative paths against framework root
                        if Path::new(path).is_relative() {
                            root.join(path)
                        } else {
                            PathBuf::from(path)
                        }
                    })
                    .filter(|p| p.exists())
                    .collect();
            }
        }

        // Fallback: scan include/ subdirectories
        let include_dir = sdk_dir.join("include");
        if !include_dir.exists() {
            return Vec::new();
        }

        let mut dirs = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&include_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    dirs.push(path.clone());
                    // Also add subdirectories (some components have nested includes)
                    if let Ok(sub_entries) = std::fs::read_dir(&path) {
                        for sub_entry in sub_entries.flatten() {
                            if sub_entry.path().is_dir() {
                                dirs.push(sub_entry.path());
                            }
                        }
                    }
                }
            }
        }
        dirs.sort();
        dirs
    }

    /// Get all precompiled `.a` library files from the ESP-IDF SDK.
    pub fn get_sdk_libs(&self, mcu: &str) -> Vec<PathBuf> {
        let lib_dir = self
            .resolved_dir()
            .join("tools")
            .join("sdk")
            .join(mcu)
            .join("lib");
        collect_archive_files(&lib_dir)
    }

    /// Get the linker scripts directory for a given MCU.
    pub fn get_linker_scripts_dir(&self, mcu: &str) -> PathBuf {
        self.resolved_dir()
            .join("tools")
            .join("sdk")
            .join(mcu)
            .join("ld")
    }

    /// Get the path to the bootloader binary.
    pub fn get_bootloader_bin(&self, mcu: &str) -> PathBuf {
        self.resolved_dir()
            .join("tools")
            .join("sdk")
            .join(mcu)
            .join("bin")
            .join("bootloader.bin")
    }

    /// Get the path to the partitions binary.
    pub fn get_partitions_bin(&self, mcu: &str) -> PathBuf {
        self.resolved_dir()
            .join("tools")
            .join("sdk")
            .join(mcu)
            .join("bin")
            .join("partitions.bin")
    }

    /// List all source files in a core directory.
    pub fn get_core_sources(&self, core_name: &str) -> Vec<PathBuf> {
        collect_sources(&self.get_core_dir(core_name))
    }
}

impl crate::Package for Esp32Framework {
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

/// Find the actual framework root inside an extracted archive.
fn find_framework_root(install_dir: &Path) -> PathBuf {
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

/// Collect all `.a` archive files from a directory (non-recursive).
fn collect_archive_files(dir: &Path) -> Vec<PathBuf> {
    let mut libs = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().is_some_and(|e| e == "a") {
                libs.push(path);
            }
        }
    }
    libs.sort();
    libs
}

/// Collect source files from a directory (non-recursive).
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
    fn test_esp32_framework_not_installed() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::env::set_var("FBUILD_CACHE_DIR", tmp.path().join("cache"));
        let fw = Esp32Framework::new(tmp.path(), "esp32c6");
        assert!(!fw.is_installed());
        std::env::remove_var("FBUILD_CACHE_DIR");
    }

    #[test]
    fn test_find_framework_root_direct() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("cores")).unwrap();
        assert_eq!(find_framework_root(tmp.path()), tmp.path().to_path_buf());
    }

    #[test]
    fn test_find_framework_root_nested() {
        let tmp = tempfile::TempDir::new().unwrap();
        let nested = tmp.path().join("framework-arduinoespressif32");
        std::fs::create_dir_all(nested.join("cores")).unwrap();
        assert_eq!(find_framework_root(tmp.path()), nested);
    }

    #[test]
    fn test_get_core_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fw = Esp32Framework::new(tmp.path(), "esp32c6");
        let core_dir = fw.get_core_dir("esp32");
        assert!(core_dir.to_string_lossy().contains("cores"));
        assert!(core_dir.to_string_lossy().contains("esp32"));
    }

    #[test]
    fn test_get_variant_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fw = Esp32Framework::new(tmp.path(), "esp32c6");
        let variant_dir = fw.get_variant_dir("esp32c6");
        assert!(variant_dir.to_string_lossy().contains("variants"));
        assert!(variant_dir.to_string_lossy().contains("esp32c6"));
    }

    #[test]
    fn test_sdk_paths() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fw = Esp32Framework::new(tmp.path(), "esp32c6");
        let ld_dir = fw.get_linker_scripts_dir("esp32c6");
        assert!(ld_dir.to_string_lossy().contains("sdk"));
        assert!(ld_dir.to_string_lossy().contains("esp32c6"));
        assert!(ld_dir.to_string_lossy().contains("ld"));
    }

    #[test]
    fn test_collect_archive_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("libfreertos.a"), "").unwrap();
        std::fs::write(tmp.path().join("libesp_system.a"), "").unwrap();
        std::fs::write(tmp.path().join("readme.txt"), "").unwrap();
        let libs = collect_archive_files(tmp.path());
        assert_eq!(libs.len(), 2);
        assert!(libs.iter().all(|p| p.extension().unwrap() == "a"));
    }

    #[test]
    fn test_get_sdk_libs_empty() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fw = Esp32Framework::new(tmp.path(), "esp32c6");
        let libs = fw.get_sdk_libs("esp32c6");
        assert!(libs.is_empty()); // No SDK installed
    }

    #[test]
    fn test_validate_missing_cores() {
        let tmp = tempfile::TempDir::new().unwrap();
        let result = Esp32Framework::validate(tmp.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_missing_arduino_h() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("cores").join("esp32")).unwrap();
        let result = Esp32Framework::validate(tmp.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Arduino.h"));
    }

    #[test]
    fn test_bootloader_bin_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fw = Esp32Framework::new(tmp.path(), "esp32c6");
        let boot = fw.get_bootloader_bin("esp32c6");
        assert!(boot.to_string_lossy().contains("bootloader.bin"));
    }

    #[test]
    fn test_partitions_bin_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fw = Esp32Framework::new(tmp.path(), "esp32c6");
        let parts = fw.get_partitions_bin("esp32c6");
        assert!(parts.to_string_lossy().contains("partitions.bin"));
    }

    #[test]
    fn test_sdk_include_dirs_with_mock() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Create mock SDK structure with includes file
        let sdk_dir = tmp.path().join("tools").join("sdk").join("esp32c6");
        let flags_dir = sdk_dir.join("flags");
        std::fs::create_dir_all(&flags_dir).unwrap();

        // Create some include dirs
        let inc1 = sdk_dir.join("include").join("freertos");
        let inc2 = sdk_dir.join("include").join("esp_system");
        std::fs::create_dir_all(&inc1).unwrap();
        std::fs::create_dir_all(&inc2).unwrap();

        // Write includes file with absolute paths
        let includes_content = format!("-I{}\n-I{}\n", inc1.display(), inc2.display());
        std::fs::write(flags_dir.join("includes"), &includes_content).unwrap();

        let fw = Esp32Framework {
            base: PackageBase::new(
                "test",
                "1.0",
                "http://example.com",
                "http://example.com",
                None,
                CacheSubdir::Platforms,
                tmp.path(),
            ),
            install_dir: Some(tmp.path().to_path_buf()),
        };

        let dirs = fw.get_sdk_include_dirs("esp32c6");
        assert_eq!(dirs.len(), 2);
    }
}
