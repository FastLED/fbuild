//! Teensy cores framework package.
//!
//! Downloads and manages the Teensy cores from PaulStoffregen/cores on GitHub.
//! Key difference from ArduinoCore: no `cores/` wrapper directory. The archive
//! extracts as `cores-master/` containing `teensy4/` directly.

use std::path::{Path, PathBuf};

use crate::{CacheSubdir, Framework, PackageBase, PackageInfo};

const TEENSY_CORE_VERSION: &str = "master";
const TEENSY_CORE_URL: &str =
    "https://github.com/PaulStoffregen/cores/archive/refs/heads/master.zip";

/// Teensy cores framework manager.
pub struct TeensyCores {
    base: PackageBase,
    install_dir: Option<PathBuf>,
}

impl TeensyCores {
    pub fn new(project_dir: &Path) -> Self {
        Self {
            base: PackageBase::new(
                "teensy-cores",
                TEENSY_CORE_VERSION,
                TEENSY_CORE_URL,
                TEENSY_CORE_URL,
                None, // No checksum for master branch
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

        // Teensy cores have teensy4/ directory directly (no cores/ wrapper)
        let teensy4 = root.join("teensy4");
        if !teensy4.exists() {
            return Err(fbuild_core::FbuildError::PackageError(format!(
                "Teensy cores missing teensy4/ directory (in {})",
                root.display()
            )));
        }

        // Check for key source files
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

        Ok(())
    }

    /// Get the core source directory for a specific core name (e.g. "teensy4").
    pub fn get_core_dir(&self, core_name: &str) -> PathBuf {
        self.resolved_dir().join(core_name)
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
}

impl crate::Package for TeensyCores {
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
        root.join("teensy4").join("Arduino.h").exists()
    }

    fn get_info(&self) -> PackageInfo {
        self.base.get_info()
    }
}

impl Framework for TeensyCores {
    /// For Teensy, cores_dir is the framework root itself (not a `cores/` subdirectory).
    fn get_cores_dir(&self) -> PathBuf {
        self.resolved_dir()
    }

    /// Teensy cores have no variants/ directory — returns the root as fallback.
    fn get_variants_dir(&self) -> PathBuf {
        self.resolved_dir()
    }

    /// Libraries directory (if present in core).
    fn get_libraries_dir(&self) -> PathBuf {
        self.resolved_dir().join("libraries")
    }
}

/// Find the actual core root inside an extracted archive.
///
/// GitHub archives extract as `cores-master/` with the core dirs inside.
fn find_core_root(install_dir: &Path) -> PathBuf {
    // Direct teensy4/ in install dir
    if install_dir.join("teensy4").exists() {
        return install_dir.to_path_buf();
    }

    // Check one level deep (e.g. cores-master/)
    if let Ok(entries) = std::fs::read_dir(install_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && path.join("teensy4").exists() {
                return path;
            }
        }
    }

    install_dir.to_path_buf()
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
                // Exclude known test files
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
        std::env::set_var("FBUILD_CACHE_DIR", tmp.path().join("cache"));
        let core = TeensyCores::new(tmp.path());
        assert!(!core.is_installed());
        std::env::remove_var("FBUILD_CACHE_DIR");
    }

    #[test]
    fn test_find_core_root_direct() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("teensy4")).unwrap();
        assert_eq!(find_core_root(tmp.path()), tmp.path().to_path_buf());
    }

    #[test]
    fn test_find_core_root_nested() {
        let tmp = tempfile::TempDir::new().unwrap();
        let nested = tmp.path().join("cores-master");
        std::fs::create_dir_all(nested.join("teensy4")).unwrap();
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
        // .cpp, .c, .S (lowercased to "s") collected; .h excluded
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
        std::fs::create_dir_all(tmp.path().join("teensy4")).unwrap();
        let result = TeensyCores::validate(tmp.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Arduino.h"));
    }
}
