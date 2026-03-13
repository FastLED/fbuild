//! Arduino AVR Core framework package.
//!
//! Downloads and manages the Arduino AVR core from GitHub.
//! Provides paths to: cores/arduino, variants/, libraries/, boards.txt.

use std::path::{Path, PathBuf};

use crate::{CacheSubdir, Framework, PackageBase, PackageInfo};

const AVR_CORE_VERSION: &str = "1.8.6";
const AVR_CORE_URL: &str =
    "https://github.com/arduino/ArduinoCore-avr/archive/refs/tags/1.8.6.tar.gz";
const AVR_CORE_CHECKSUM: &str = "49241fd5e504482b94954b5843c7d69ce38ebc1ab47ad3b677e8bb77e0cb8fe6";

/// Arduino AVR Core framework manager.
pub struct ArduinoCore {
    base: PackageBase,
    install_dir: Option<PathBuf>,
}

impl ArduinoCore {
    pub fn new(project_dir: &Path) -> Self {
        Self {
            base: PackageBase::new(
                "arduino-avr-core",
                AVR_CORE_VERSION,
                AVR_CORE_URL,
                AVR_CORE_URL,
                Some(AVR_CORE_CHECKSUM),
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

        let required = ["cores/arduino", "variants", "boards.txt"];

        for path in &required {
            if !root.join(path).exists() {
                return Err(fbuild_core::FbuildError::PackageError(format!(
                    "Arduino core missing required path: {} (in {})",
                    path,
                    root.display()
                )));
            }
        }

        // Check for key source files
        let main_cpp = root.join("cores/arduino/main.cpp");
        if !main_cpp.exists() {
            return Err(fbuild_core::FbuildError::PackageError(
                "Arduino core missing cores/arduino/main.cpp".to_string(),
            ));
        }

        Ok(())
    }

    /// Get path to boards.txt.
    pub fn get_boards_txt(&self) -> PathBuf {
        self.resolved_dir().join("boards.txt")
    }

    /// Get path to platform.txt.
    pub fn get_platform_txt(&self) -> PathBuf {
        self.resolved_dir().join("platform.txt")
    }

    /// Get the core source directory for a specific core name.
    pub fn get_core_dir(&self, core_name: &str) -> PathBuf {
        self.get_cores_dir().join(core_name)
    }

    /// Get the variant directory for a specific variant name.
    pub fn get_variant_dir(&self, variant_name: &str) -> PathBuf {
        self.get_variants_dir().join(variant_name)
    }

    /// List all .c and .cpp source files in the core.
    pub fn get_core_sources(&self, core_name: &str) -> Vec<PathBuf> {
        let core_dir = self.get_core_dir(core_name);
        collect_sources(&core_dir)
    }

    /// List all .c and .cpp source files in a variant.
    pub fn get_variant_sources(&self, variant_name: &str) -> Vec<PathBuf> {
        let variant_dir = self.get_variant_dir(variant_name);
        collect_sources(&variant_dir)
    }
}

impl crate::Package for ArduinoCore {
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
        root.join("cores").join("arduino").exists()
    }

    fn get_info(&self) -> PackageInfo {
        self.base.get_info()
    }
}

impl Framework for ArduinoCore {
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
/// GitHub archives extract as `ArduinoCore-avr-1.8.6/` with the core inside.
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

/// Collect .c and .cpp source files from a directory (non-recursive).
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
                if matches!(ext.as_str(), "c" | "cpp" | "s") {
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
    fn test_arduino_core_not_installed() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Use isolated cache so global cache doesn't interfere
        std::env::set_var("FBUILD_CACHE_DIR", tmp.path().join("cache"));
        let core = ArduinoCore::new(tmp.path());
        assert!(!core.is_installed());
        std::env::remove_var("FBUILD_CACHE_DIR");
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
        let nested = tmp.path().join("ArduinoCore-avr-1.8.6");
        std::fs::create_dir_all(nested.join("cores/arduino")).unwrap();
        assert_eq!(find_core_root(tmp.path()), nested);
    }

    #[test]
    fn test_get_boards_txt() {
        let tmp = tempfile::TempDir::new().unwrap();
        let core = ArduinoCore::new(tmp.path());
        let path = core.get_boards_txt();
        assert!(path.to_string_lossy().contains("boards.txt"));
    }

    #[test]
    fn test_collect_sources() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("main.cpp"), "").unwrap();
        std::fs::write(tmp.path().join("wiring.c"), "").unwrap();
        std::fs::write(tmp.path().join("header.h"), "").unwrap();
        let sources = collect_sources(tmp.path());
        assert_eq!(sources.len(), 2); // .cpp and .c, not .h
    }

    #[test]
    fn test_validate_missing_cores() {
        let tmp = tempfile::TempDir::new().unwrap();
        let result = ArduinoCore::validate(tmp.path());
        assert!(result.is_err());
    }
}
