//! Data-driven AVR Arduino framework resolver.
//!
//! Reads `avr_frameworks.json` to map board core names (e.g., "arduino", "tiny")
//! to the correct framework package (GitHub URL, version, validation path).
//! This mirrors PlatformIO's platform-atmelavr mapping without hardcoding URLs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::{CacheSubdir, Framework, PackageBase, PackageInfo};

/// Embedded framework registry (compile-time).
const AVR_FRAMEWORKS_JSON: &str = include_str!("../../assets/avr_frameworks.json");

/// Metadata for a single AVR framework package.
#[derive(Debug, Clone)]
struct FrameworkEntry {
    name: String,
    github: String,
    version: String,
    tag_prefix: String,
    checksum: Option<String>,
    validation_path: String,
    /// Override for the core subdirectory name inside `cores/`.
    /// When `None`, the registry key (core name) is used as the directory name.
    core_dir: Option<String>,
    /// Whether this framework needs ArduinoCore-API injected into
    /// `cores/<core>/api/`.
    needs_arduino_api: bool,
}

/// Parse the embedded JSON registry.
fn load_registry() -> HashMap<String, FrameworkEntry> {
    let parsed: serde_json::Value =
        serde_json::from_str(AVR_FRAMEWORKS_JSON).expect("avr_frameworks.json is invalid JSON");

    let frameworks = parsed
        .get("frameworks")
        .and_then(|v| v.as_object())
        .expect("avr_frameworks.json missing 'frameworks' object");

    let mut map = HashMap::new();
    for (core_name, entry) in frameworks {
        let name = entry["name"].as_str().unwrap_or("").to_string();
        let github = entry["github"].as_str().unwrap_or("").to_string();
        let version = entry["version"].as_str().unwrap_or("").to_string();
        let tag_prefix = entry["tag_prefix"].as_str().unwrap_or("").to_string();
        let checksum = entry["checksum"].as_str().map(|s| s.to_string());
        let validation_path = entry["validation_path"].as_str().unwrap_or("").to_string();
        let core_dir = entry["core_dir"].as_str().map(|s| s.to_string());
        let needs_arduino_api = entry["needs_arduino_api"].as_bool().unwrap_or(false);

        map.insert(
            core_name.clone(),
            FrameworkEntry {
                name,
                github,
                version,
                tag_prefix,
                checksum,
                validation_path,
                core_dir,
                needs_arduino_api,
            },
        );
    }
    map
}

/// Look up the framework entry for a given core name.
fn lookup_entry(core_name: &str) -> fbuild_core::Result<FrameworkEntry> {
    let registry = load_registry();
    registry.get(core_name).cloned().ok_or_else(|| {
        let available: Vec<&str> = registry.keys().map(|s| s.as_str()).collect();
        fbuild_core::FbuildError::ConfigError(format!(
            "no AVR framework registered for core '{}' (available: {:?})",
            core_name, available
        ))
    })
}

/// Data-driven AVR framework manager.
///
/// Resolves the correct Arduino framework for any AVR board core
/// by reading from the embedded `avr_frameworks.json` registry.
pub struct AvrFramework {
    base: PackageBase,
    core_name: String,
    validation_path: String,
    /// Override for the subdirectory name inside `cores/`.
    /// When `None`, `core_name` is used (works for most frameworks).
    core_dir_override: Option<String>,
    /// Whether to fetch ArduinoCore-API into `cores/<core>/api/` after install.
    needs_arduino_api: bool,
}

impl AvrFramework {
    /// Create a framework manager for the given board core name.
    ///
    /// The core name comes from the board JSON (e.g., "arduino", "tiny", "tinymodern").
    pub fn for_core(core_name: &str, project_dir: &Path) -> fbuild_core::Result<Self> {
        let entry = lookup_entry(core_name)?;
        let url = format!(
            "https://github.com/{}/archive/refs/tags/{}{}.tar.gz",
            entry.github, entry.tag_prefix, entry.version
        );

        Ok(Self {
            base: PackageBase::new(
                &entry.name,
                &entry.version,
                &url,
                &url,
                entry.checksum.as_deref(),
                CacheSubdir::Platforms,
                project_dir,
            ),
            core_name: core_name.to_string(),
            validation_path: entry.validation_path,
            core_dir_override: entry.core_dir,
            needs_arduino_api: entry.needs_arduino_api,
        })
    }

    /// Get the resolved root directory of the framework.
    fn resolved_dir(&self) -> PathBuf {
        find_framework_root(&self.base.install_path())
    }

    /// Get the core source directory for a specific core name.
    ///
    /// Uses `core_dir` from avr_frameworks.json when set (e.g. MiniCore uses
    /// `MCUdude_corefiles` instead of `MiniCore` as the directory name).
    pub fn get_core_dir(&self, core_name: &str) -> PathBuf {
        let dir_name = self.core_dir_override.as_deref().unwrap_or(core_name);
        self.get_cores_dir().join(dir_name)
    }

    /// Get the variant directory for a specific variant name.
    pub fn get_variant_dir(&self, variant_name: &str) -> PathBuf {
        self.get_variants_dir().join(variant_name)
    }
}

impl crate::Package for AvrFramework {
    fn ensure_installed(&self) -> fbuild_core::Result<PathBuf> {
        if self.is_installed() {
            let root = self.resolved_dir();
            // Still ensure API is present (may have been cached without it)
            if self.needs_arduino_api {
                let core_dir = self.get_core_dir(&self.core_name);
                super::arduino_api::ensure_arduino_api(&core_dir)?;
            }
            return Ok(root);
        }

        let validation_path = self.validation_path.clone();
        let core_name = self.core_name.clone();
        let validate_fn = move |install_dir: &Path| {
            let root = find_framework_root(install_dir);
            if !validation_path.is_empty() {
                let required = root.join(&validation_path);
                if !required.exists() {
                    return Err(fbuild_core::FbuildError::PackageError(format!(
                        "AVR framework '{}' missing required path: {} (in {})",
                        core_name,
                        validation_path,
                        root.display()
                    )));
                }
            }
            Ok(())
        };

        let rt = tokio::runtime::Handle::try_current().ok();
        let install_path = if let Some(handle) = rt {
            handle.block_on(self.base.staged_install(validate_fn))?
        } else {
            let rt = tokio::runtime::Runtime::new().map_err(|e| {
                fbuild_core::FbuildError::PackageError(format!(
                    "failed to create tokio runtime: {}",
                    e
                ))
            })?;
            rt.block_on(self.base.staged_install(validate_fn))?
        };

        let root = find_framework_root(&install_path);

        // Fetch ArduinoCore-API if needed (e.g. ArduinoCore-megaavr)
        if self.needs_arduino_api {
            let core_dir_name = self.core_dir_override.as_deref().unwrap_or(&self.core_name);
            let core_dir = root.join("cores").join(core_dir_name);
            super::arduino_api::ensure_arduino_api(&core_dir)?;
        }

        Ok(root)
    }

    fn is_installed(&self) -> bool {
        if !self.base.is_cached() {
            return false;
        }
        let root = find_framework_root(&self.base.install_path());
        root.join("cores").exists()
    }

    fn get_info(&self) -> PackageInfo {
        self.base.get_info()
    }
}

impl Framework for AvrFramework {
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
///
/// GitHub archives can have nested structures:
/// - `RepoName-version/cores/` (standard Arduino)
/// - `RepoName-version/avr/cores/` (ATTinyCore)
///   Searches up to two levels deep for a `cores/` directory.
fn find_framework_root(install_dir: &Path) -> PathBuf {
    if install_dir.join("cores").exists() {
        return install_dir.to_path_buf();
    }

    // Check one and two levels deep
    if let Ok(entries) = std::fs::read_dir(install_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.join("cores").exists() {
                    return path;
                }
                // Check two levels deep (e.g., ATTinyCore-1.5.2/avr/)
                if let Ok(sub_entries) = std::fs::read_dir(&path) {
                    for sub_entry in sub_entries.flatten() {
                        let sub_path = sub_entry.path();
                        if sub_path.is_dir() && sub_path.join("cores").exists() {
                            return sub_path;
                        }
                    }
                }
            }
        }
    }

    install_dir.to_path_buf()
}

/// Check if a given core name has a registered framework.
pub fn is_registered_core(core_name: &str) -> bool {
    load_registry().contains_key(core_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_loads() {
        let registry = load_registry();
        assert!(registry.contains_key("arduino"));
        assert!(registry.contains_key("tiny"));
        assert!(registry.contains_key("tinymodern"));
    }

    #[test]
    fn test_arduino_entry() {
        let entry = lookup_entry("arduino").unwrap();
        assert_eq!(entry.name, "arduino-avr-core");
        assert!(entry.github.contains("ArduinoCore-avr"));
    }

    #[test]
    fn test_tiny_entry() {
        let entry = lookup_entry("tiny").unwrap();
        assert_eq!(entry.name, "attiny-core");
        assert!(entry.github.contains("ATTinyCore"));
    }

    #[test]
    fn test_unknown_core_fails() {
        assert!(lookup_entry("nonexistent").is_err());
    }

    #[test]
    fn test_tiny_and_tinymodern_share_repo() {
        let tiny = lookup_entry("tiny").unwrap();
        let tinymodern = lookup_entry("tinymodern").unwrap();
        assert_eq!(tiny.github, tinymodern.github);
        assert_eq!(tiny.version, tinymodern.version);
    }

    #[test]
    fn test_minicore_registered() {
        let registry = load_registry();
        assert!(
            registry.contains_key("MiniCore"),
            "MiniCore must be registered"
        );
    }

    #[test]
    fn test_minicore_lookup() {
        let entry = lookup_entry("MiniCore").unwrap();
        assert!(entry.github.contains("MiniCore"));
        assert!(!entry.version.is_empty());
        assert!(!entry.validation_path.is_empty());
    }

    #[test]
    fn test_megatinycore_registered() {
        let registry = load_registry();
        assert!(
            registry.contains_key("megatinycore"),
            "megatinycore must be registered"
        );
    }

    #[test]
    fn test_megatinycore_lookup() {
        let entry = lookup_entry("megatinycore").unwrap();
        assert!(entry.github.contains("megaTinyCore"));
        assert_eq!(entry.version, "2.6.11");
        assert!(entry.validation_path.contains("megatinycore"));
        assert_eq!(entry.core_dir.as_deref(), Some("megatinycore"));
    }

    #[test]
    fn test_megacorex_registered() {
        let registry = load_registry();
        assert!(
            registry.contains_key("MegaCoreX"),
            "MegaCoreX must be registered"
        );
    }

    #[test]
    fn test_megacorex_lookup() {
        let entry = lookup_entry("MegaCoreX").unwrap();
        assert!(entry.github.contains("MegaCoreX"));
        assert_eq!(entry.version, "1.1.5");
        assert_eq!(entry.tag_prefix, "v");
        assert_eq!(entry.core_dir.as_deref(), Some("coreX-corefiles"));
    }

    #[test]
    fn test_arduino_megaavr_registered() {
        let registry = load_registry();
        assert!(
            registry.contains_key("arduino_megaavr"),
            "arduino_megaavr must be registered for AtmelMegaAvr boards"
        );
    }

    #[test]
    fn test_arduino_megaavr_lookup() {
        let entry = lookup_entry("arduino_megaavr").unwrap();
        assert!(entry.github.contains("ArduinoCore-megaavr"));
        assert_eq!(entry.name, "arduino-megaavr-core");
        assert_eq!(entry.core_dir.as_deref(), Some("arduino"));
    }
}
