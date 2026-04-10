//! Arduino mbed core (ArduinoCore-mbed) framework package.
//!
//! Downloads the official ArduinoCore-mbed archive from GitHub tags.
//! The tag archive includes the prebuilt per-variant `libmbed.a` payloads
//! used by the STM32 H7 Arduino boards (Giga, Portenta, Opta, Nicla Vision).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::{CacheSubdir, Framework, PackageBase, PackageInfo};

const ARDUINO_MBED_CORE_VERSION: &str = "4.5.0";
const ARDUINO_MBED_CORE_URL: &str =
    "https://github.com/arduino/ArduinoCore-mbed/archive/refs/tags/4.5.0.tar.gz";

/// ArduinoCore-mbed framework manager.
pub struct ArduinoMbedCore {
    base: PackageBase,
    install_dir: Option<PathBuf>,
}

impl ArduinoMbedCore {
    pub fn new(project_dir: &Path) -> Self {
        Self {
            base: PackageBase::new(
                "arduino-mbed-core",
                ARDUINO_MBED_CORE_VERSION,
                ARDUINO_MBED_CORE_URL,
                ARDUINO_MBED_CORE_URL,
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
                "arduino-mbed-core",
                ARDUINO_MBED_CORE_VERSION,
                ARDUINO_MBED_CORE_URL,
                ARDUINO_MBED_CORE_URL,
                None,
                CacheSubdir::Platforms,
                project_dir,
                cache_root,
            ),
            install_dir: None,
        }
    }

    fn resolved_dir(&self) -> PathBuf {
        self.install_dir
            .clone()
            .unwrap_or_else(|| find_core_root(&self.base.install_path()))
    }

    fn validate(install_dir: &Path) -> fbuild_core::Result<()> {
        let root = find_core_root(install_dir);

        let arduino_h = root.join("cores/arduino/Arduino.h");
        if !arduino_h.exists() {
            return Err(fbuild_core::FbuildError::PackageError(format!(
                "Arduino mbed core missing cores/arduino/Arduino.h (in {})",
                root.display()
            )));
        }

        Ok(())
    }

    pub fn get_core_dir(&self, core_name: &str) -> PathBuf {
        self.get_cores_dir().join(core_name)
    }

    pub fn get_variant_dir(&self, variant_name: &str) -> PathBuf {
        self.get_variants_dir().join(variant_name)
    }

    pub fn get_linker_script(&self, variant_name: &str) -> PathBuf {
        self.get_variant_dir(variant_name).join("linker_script.ld")
    }

    pub fn get_mbed_lib(&self, variant_name: &str) -> PathBuf {
        self.get_variant_dir(variant_name)
            .join("libs")
            .join("libmbed.a")
    }

    pub fn read_variant_file(&self, variant_name: &str, filename: &str) -> String {
        let path = self.get_variant_dir(variant_name).join(filename);
        std::fs::read_to_string(path).unwrap_or_default()
    }

    /// Resolve `includes.txt` entries to absolute include directories.
    ///
    /// ArduinoCore-mbed uses `-iprefix {build.core.path}` and emits
    /// `-iwithprefixbefore/...` lines relative to `cores/arduino`.
    pub fn get_variant_includes(&self, variant_name: &str) -> Vec<PathBuf> {
        let variant_dir = self.get_variant_dir(variant_name);
        let includes_txt = variant_dir.join("includes.txt");
        let core_dir = self.get_core_dir("arduino");

        let content = match std::fs::read_to_string(&includes_txt) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };

        let mut seen = HashSet::new();
        let mut includes = Vec::new();
        for line in content.lines() {
            let line = line.trim();
            let rel = line
                .strip_prefix("-iwithprefixbefore/")
                .or_else(|| line.strip_prefix("-iwithprefixbefore"));
            let Some(rel) = rel else {
                continue;
            };
            let path = core_dir.join(rel.trim_start_matches('/'));
            if path.is_dir() && seen.insert(path.clone()) {
                includes.push(path);
            }
        }

        includes
    }

    /// ArduinoCore-mbed compiles a small wrapper core and links against a
    /// prebuilt `libmbed.a`. Avoid recursively scanning the full `mbed/` tree.
    pub fn get_core_sources(&self) -> Vec<PathBuf> {
        let core_dir = self.get_core_dir("arduino");
        let mut sources = Vec::new();
        let mut seen = HashSet::new();

        collect_sources_non_recursive(&core_dir, &mut sources, &mut seen);
        for subdir in ["api", "USB", "as_mbed_library"] {
            collect_sources_recursive(&core_dir.join(subdir), &mut sources, &mut seen);
        }

        let mstd_mutex = core_dir
            .join("mbed")
            .join("platform")
            .join("cxxsupport")
            .join("mstd_mutex.cpp");
        if mstd_mutex.is_file() && seen.insert(mstd_mutex.clone()) {
            sources.push(mstd_mutex);
        }

        sources.sort();
        sources
    }

    pub fn get_variant_sources(&self, variant_name: &str) -> Vec<PathBuf> {
        let variant_dir = self.get_variant_dir(variant_name);
        let mut sources = Vec::new();
        let mut seen = HashSet::new();
        collect_sources_non_recursive(&variant_dir, &mut sources, &mut seen);
        sources.sort();
        sources
    }
}

impl crate::Package for ArduinoMbedCore {
    fn ensure_installed(&self) -> fbuild_core::Result<PathBuf> {
        if self.is_installed() {
            let root = self.resolved_dir();
            super::arduino_api::ensure_arduino_api(&root.join("cores").join("arduino"))?;
            return Ok(root);
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

        let root = find_core_root(&install_path);
        super::arduino_api::ensure_arduino_api(&root.join("cores").join("arduino"))?;
        Ok(root)
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

impl Framework for ArduinoMbedCore {
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

fn collect_sources_non_recursive(
    dir: &Path,
    sources: &mut Vec<PathBuf>,
    seen: &mut HashSet<PathBuf>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && is_source_file(&path) && seen.insert(path.clone()) {
            sources.push(path);
        }
    }
}

fn collect_sources_recursive(dir: &Path, sources: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_sources_recursive(&path, sources, seen);
        } else if is_source_file(&path) && seen.insert(path.clone()) {
            sources.push(path);
        }
    }
}

fn is_source_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| matches!(e.to_ascii_lowercase().as_str(), "c" | "cc" | "cpp" | "s"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Package;

    #[test]
    fn test_arduino_mbed_core_not_installed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let core = ArduinoMbedCore::with_cache_root(tmp.path(), &tmp.path().join("cache"));
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
        let nested = tmp.path().join("ArduinoCore-mbed-4.5.0");
        std::fs::create_dir_all(nested.join("cores/arduino")).unwrap();
        assert_eq!(find_core_root(tmp.path()), nested);
    }
}
