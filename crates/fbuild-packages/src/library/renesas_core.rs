//! Arduino Renesas core (ArduinoCore-renesas) framework package.
//!
//! Downloads the official Arduino Board Manager archive which includes
//! all submodules pre-resolved (ArduinoCore-API, TinyUSB, FSP, etc.).
//! GitHub archive downloads exclude submodules, so we use the Arduino
//! Board Manager URL instead.

use std::path::{Path, PathBuf};

use crate::{CacheSubdir, Framework, PackageBase, PackageInfo};

const RENESAS_CORE_VERSION: &str = "1.2.2";
/// Arduino Board Manager archive — includes all submodules resolved.
/// GitHub archive URLs exclude submodules (tinyusb, fsp, ArduinoCore-API are symlinks).
const RENESAS_CORE_URL: &str =
    "https://downloads.arduino.cc/cores/staging/ArduinoCore-renesas_uno-1.2.2.tar.bz2";

/// Arduino Renesas core framework manager.
pub struct RenesasCores {
    base: PackageBase,
    install_dir: Option<PathBuf>,
}

impl RenesasCores {
    pub fn new(project_dir: &Path) -> Self {
        Self {
            base: PackageBase::new(
                "renesas-core",
                RENESAS_CORE_VERSION,
                RENESAS_CORE_URL,
                RENESAS_CORE_URL,
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
                "renesas-core",
                RENESAS_CORE_VERSION,
                RENESAS_CORE_URL,
                RENESAS_CORE_URL,
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
                "Renesas core missing cores/arduino/Arduino.h (in {})",
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

    /// Get additional include directories from the variant's `includes.txt`.
    ///
    /// ArduinoCore-renesas variants ship an `includes.txt` that lists all
    /// FSP (Flexible Software Package) include paths needed for compilation.
    /// Lines are `-iwithprefixbefore/variants/VARIANT/includes/...` entries
    /// relative to the framework root directory.
    pub fn get_variant_includes(&self, variant_name: &str) -> Vec<PathBuf> {
        let variant_dir = self.get_variant_dir(variant_name);
        let includes_txt = variant_dir.join("includes.txt");
        let framework_root = self.resolved_dir();

        let content = match std::fs::read_to_string(&includes_txt) {
            Ok(c) => c,
            Err(_) => {
                tracing::warn!("No includes.txt found at {}", includes_txt.display());
                return Vec::new();
            }
        };

        let includes: Vec<PathBuf> = content
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                // Lines: -iwithprefixbefore/variants/UNOWIFIR4/includes/ra/fsp/inc/api
                // Paths are relative to the framework root.
                let path = line
                    .strip_prefix("-iwithprefixbefore/")
                    .or_else(|| line.strip_prefix("-iwithprefixbefore"));
                path.map(|p| framework_root.join(p))
            })
            .filter(|p| p.is_dir())
            .collect();

        tracing::info!(
            "Renesas variant '{}' includes: {} paths from includes.txt",
            variant_name,
            includes.len()
        );
        includes
    }

    /// Get the linker script for a variant.
    ///
    /// Renesas variants typically have `fsp.ld` or other .ld files. This
    /// method searches the variant directory for .ld files.
    pub fn get_linker_script(&self, variant_name: &str) -> PathBuf {
        let variant_dir = self.get_variant_dir(variant_name);

        // Search for .ld files in the variant directory
        if let Some(ld) = find_ld_file(&variant_dir) {
            return ld;
        }

        // Default fallback
        variant_dir.join("fsp.ld")
    }

    /// List all .c, .cpp, .cc, and .s source files in the core.
    pub fn get_core_sources(&self, core_name: &str) -> Vec<PathBuf> {
        let core_dir = self.get_core_dir(core_name);
        collect_sources(&core_dir)
    }
}

#[async_trait::async_trait]
impl crate::Package for RenesasCores {
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

impl Framework for RenesasCores {
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
/// GitHub archives extract as `ArduinoCore-renesas-1.2.2/` with the core inside.
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

    #[test]
    fn test_renesas_cores_not_installed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let core = RenesasCores::with_cache_root(tmp.path(), &tmp.path().join("cache"));
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
        let nested = tmp.path().join("ArduinoCore-renesas-1.2.2");
        std::fs::create_dir_all(nested.join("cores/arduino")).unwrap();
        assert_eq!(find_core_root(tmp.path()), nested);
    }

    #[test]
    fn test_get_linker_script_with_ld_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let variant_dir = tmp.path().join("variants/UNOWIFIR4");
        std::fs::create_dir_all(&variant_dir).unwrap();
        std::fs::write(variant_dir.join("fsp.ld"), "").unwrap();

        // Test find_ld_file directly
        let ld = find_ld_file(&variant_dir);
        assert!(ld.is_some());
        assert!(ld.unwrap().to_string_lossy().contains("fsp.ld"));
    }

    #[test]
    fn test_get_linker_script_fallback() {
        let tmp = tempfile::TempDir::new().unwrap();
        let core = RenesasCores::new(tmp.path());
        let script = core.get_linker_script("UNOWIFIR4");
        assert!(script.to_string_lossy().contains("fsp.ld"));
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
        let result = RenesasCores::validate(tmp.path());
        assert!(result.is_err());
    }
}
