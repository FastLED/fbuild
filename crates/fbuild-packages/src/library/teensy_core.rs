//! Teensy Arduino framework package.
//!
//! Downloads and manages PlatformIO's `framework-arduinoteensy` package, which
//! contains the Teensy cores plus Teensyduino framework libraries such as SPI
//! and OctoWS2811.

use std::path::{Path, PathBuf};

use crate::library::framework_library::{discover_framework_libraries, FrameworkLibrary};
use crate::{CacheSubdir, Framework, PackageBase, PackageInfo};

/// Framework package used by platform-teensy 5.1.0.
const TEENSY_CORE_VERSION: &str = "1.160.0";
const TEENSY_CORE_URL: &str = "https://dl.registry.platformio.org/download/platformio/tool/framework-arduinoteensy/1.160.0/framework-arduinoteensy-1.160.0.tar.gz";

/// Teensy cores framework manager.
pub struct TeensyCores {
    base: PackageBase,
    install_dir: Option<PathBuf>,
}

impl TeensyCores {
    pub fn new(project_dir: &Path) -> Self {
        Self {
            base: PackageBase::new(
                "framework-arduinoteensy",
                TEENSY_CORE_VERSION,
                TEENSY_CORE_URL,
                TEENSY_CORE_URL,
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
                "framework-arduinoteensy",
                TEENSY_CORE_VERSION,
                TEENSY_CORE_URL,
                TEENSY_CORE_URL,
                None,
                CacheSubdir::Platforms,
                project_dir,
                cache_root,
            ),
            install_dir: None,
        }
    }

    /// Get the resolved root directory of the framework.
    fn resolved_dir(&self) -> PathBuf {
        self.install_dir
            .clone()
            .unwrap_or_else(|| find_core_root(&self.base.install_path()))
    }

    /// Validate the extracted framework has required structure.
    fn validate(install_dir: &Path) -> fbuild_core::Result<()> {
        let root = find_core_root(install_dir);

        let teensy4 = core_dir_for_root(&root, "teensy4");
        if !teensy4.exists() {
            return Err(fbuild_core::FbuildError::PackageError(format!(
                "Teensy framework missing teensy4 core directory (in {})",
                root.display()
            )));
        }

        let arduino_h = teensy4.join("Arduino.h");
        if !arduino_h.exists() {
            return Err(fbuild_core::FbuildError::PackageError(
                "Teensy cores missing teensy4/Arduino.h".to_string(),
            ));
        }

        let main_cpp = teensy4.join("main.cpp");
        if !main_cpp.exists() {
            return Err(fbuild_core::FbuildError::PackageError(
                "Teensy cores missing teensy4/main.cpp".to_string(),
            ));
        }

        let libraries_dir = root.join("libraries");
        if !libraries_dir.join("SPI").join("SPI.h").exists() {
            return Err(fbuild_core::FbuildError::PackageError(
                "Teensy framework missing libraries/SPI/SPI.h".to_string(),
            ));
        }
        if !libraries_dir
            .join("OctoWS2811")
            .join("OctoWS2811.h")
            .exists()
        {
            return Err(fbuild_core::FbuildError::PackageError(
                "Teensy framework missing libraries/OctoWS2811/OctoWS2811.h".to_string(),
            ));
        }

        Ok(())
    }

    /// Get the core source directory for a specific core name (e.g. "teensy4").
    pub fn get_core_dir(&self, core_name: &str) -> PathBuf {
        self.get_cores_dir().join(core_name)
    }

    /// Get the linker script for a board.
    ///
    /// Teensy 4.0 uses `imxrt1062.ld`, Teensy 4.1 uses `imxrt1062_t41.ld`.
    pub fn get_linker_script(&self, board_id: &str) -> PathBuf {
        let core_dir = self.get_core_dir("teensy4");
        match board_id {
            "teensy41" => core_dir.join("imxrt1062_t41.ld"),
            _ => core_dir.join("imxrt1062.ld"),
        }
    }

    /// List all .c and .cpp source files in a core directory.
    pub fn get_core_sources(&self, core_name: &str) -> Vec<PathBuf> {
        let core_dir = self.get_core_dir(core_name);
        collect_sources(&core_dir)
    }

    /// List bundled Teensyduino framework libraries.
    pub fn get_framework_libraries(&self) -> Vec<FrameworkLibrary> {
        discover_framework_libraries(&self.get_libraries_dir())
    }

    /// All include directories needed to make bundled framework headers visible.
    pub fn get_framework_library_include_dirs(&self) -> Vec<PathBuf> {
        self.get_framework_libraries()
            .into_iter()
            .flat_map(|lib| lib.include_dirs)
            .collect()
    }
}

#[async_trait::async_trait]
impl crate::Package for TeensyCores {
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
        core_dir_for_root(&root, "teensy4")
            .join("Arduino.h")
            .exists()
            && root.join("libraries").join("SPI").join("SPI.h").exists()
    }

    fn get_info(&self) -> PackageInfo {
        self.base.get_info()
    }
}

impl Framework for TeensyCores {
    fn get_cores_dir(&self) -> PathBuf {
        let root = self.resolved_dir();
        let cores = root.join("cores");
        if cores.is_dir() {
            cores
        } else {
            root
        }
    }

    /// Teensy cores have no variants/ directory; returns the framework root as fallback.
    fn get_variants_dir(&self) -> PathBuf {
        self.resolved_dir()
    }

    fn get_libraries_dir(&self) -> PathBuf {
        self.resolved_dir().join("libraries")
    }
}

/// Find the actual framework root inside an extracted archive.
///
/// PlatformIO framework archives extract with files directly at the root, while
/// older GitHub core archives used a single nested directory.
fn find_core_root(install_dir: &Path) -> PathBuf {
    if install_dir.join("cores").join("teensy4").exists() || install_dir.join("teensy4").exists() {
        return install_dir.to_path_buf();
    }

    if let Ok(entries) = std::fs::read_dir(install_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir()
                && (path.join("cores").join("teensy4").exists() || path.join("teensy4").exists())
            {
                return path;
            }
        }
    }

    install_dir.to_path_buf()
}

fn core_dir_for_root(root: &Path, core_name: &str) -> PathBuf {
    let nested = root.join("cores").join(core_name);
    if nested.exists() {
        nested
    } else {
        root.join(core_name)
    }
}

/// Files to exclude from core compilation.
/// Blink.cc is a test sketch in the Teensy core that defines setup()/loop().
const EXCLUDED_CORE_FILES: &[&str] = &["Blink.cc"];

/// Collect .c, .cpp, .cc, and .S source files from a directory (non-recursive).
/// Excludes known test/example files that would conflict with user sketches.
fn collect_sources(dir: &Path) -> Vec<PathBuf> {
    let mut sources = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                let filename = path.file_name().unwrap_or_default().to_string_lossy();
                if EXCLUDED_CORE_FILES.contains(&filename.as_ref()) {
                    continue;
                }

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
    fn test_teensy_cores_not_installed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let core = TeensyCores::with_cache_root(tmp.path(), &tmp.path().join("cache"));
        assert!(!core.is_installed());
    }

    #[test]
    fn test_find_core_root_direct() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("cores").join("teensy4")).unwrap();
        assert_eq!(find_core_root(tmp.path()), tmp.path().to_path_buf());
    }

    #[test]
    fn test_find_core_root_nested() {
        let tmp = tempfile::TempDir::new().unwrap();
        let nested = tmp.path().join("framework-arduinoteensy");
        std::fs::create_dir_all(nested.join("cores").join("teensy4")).unwrap();
        assert_eq!(find_core_root(tmp.path()), nested);
    }

    #[test]
    fn test_get_linker_script_teensy40() {
        let tmp = tempfile::TempDir::new().unwrap();
        let core = TeensyCores::new(tmp.path());
        let script = core.get_linker_script("teensy40");
        assert!(script.to_string_lossy().contains("imxrt1062.ld"));
        assert!(!script.to_string_lossy().contains("t41"));
    }

    #[test]
    fn test_get_linker_script_teensy41() {
        let tmp = tempfile::TempDir::new().unwrap();
        let core = TeensyCores::new(tmp.path());
        let script = core.get_linker_script("teensy41");
        assert!(script.to_string_lossy().contains("imxrt1062_t41.ld"));
    }

    #[test]
    fn test_collect_sources() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("main.cpp"), "").unwrap();
        std::fs::write(tmp.path().join("wiring.c"), "").unwrap();
        std::fs::write(tmp.path().join("startup.S"), "").unwrap();
        std::fs::write(tmp.path().join("Arduino.h"), "").unwrap();
        let sources = collect_sources(tmp.path());
        assert_eq!(sources.len(), 3);
    }

    #[test]
    fn test_validate_missing_teensy4() {
        let tmp = tempfile::TempDir::new().unwrap();
        let result = TeensyCores::validate(tmp.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_missing_arduino_h() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("cores").join("teensy4")).unwrap();
        let result = TeensyCores::validate(tmp.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Arduino.h"));
    }
}
