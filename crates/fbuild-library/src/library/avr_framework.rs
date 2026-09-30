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
    url: Option<String>,
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
        let url = entry["url"].as_str().map(str::to_string);
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
                url,
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

fn framework_url(entry: &FrameworkEntry) -> String {
    entry.url.clone().unwrap_or_else(|| {
        format!(
            "https://github.com/{}/archive/refs/tags/{}{}.tar.gz",
            entry.github, entry.tag_prefix, entry.version
        )
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
        let url = framework_url(&entry);

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

    /// Construct with a consumer-supplied override (parsed from the env's
    /// `platform_packages` line in `platformio.ini`). The default URL / version
    /// / checksum resolved from `avr_frameworks.json` are replaced; `cache_subdir`
    /// and `name` are preserved. See `PackageBase::with_override` and
    /// FastLED/fbuild#667 (AVR) / #669 (ATtiny).
    ///
    /// This is the JSON-driven analog of the per-framework `with_override`
    /// constructors used by other Arduino cores (e.g. `ArduinoCoreLpc8xx::with_override`).
    /// The `core_name` argument is resolved against the same registry as
    /// `for_core`; the override is applied to the resulting `PackageBase`
    /// before the struct is returned.
    pub fn for_core_with_override(
        core_name: &str,
        project_dir: &Path,
        ovr: fbuild_config::PackageOverride,
    ) -> fbuild_core::Result<Self> {
        let entry = lookup_entry(core_name)?;
        let url = framework_url(&entry);
        // PlatformIO repacks MiniCore with `cores/MiniCore`, while its
        // upstream GitHub tag uses `cores/MCUdude_corefiles`. Preserve the
        // GitHub/URL-override layout unless the resolved payload is the
        // PlatformIO package archive.
        let platformio_minicore =
            core_name == "MiniCore" && ovr.url.contains("/tool/framework-arduino-avr-minicore/");
        let validation_path = if platformio_minicore {
            "cores/MiniCore/Arduino.h".to_string()
        } else {
            entry.validation_path
        };
        let core_dir_override = if platformio_minicore {
            Some("MiniCore".to_string())
        } else {
            entry.core_dir
        };

        Ok(Self {
            base: PackageBase::new(
                &entry.name,
                &entry.version,
                &url,
                &url,
                entry.checksum.as_deref(),
                CacheSubdir::Platforms,
                project_dir,
            )
            .with_override(ovr),
            core_name: core_name.to_string(),
            validation_path,
            core_dir_override,
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

#[async_trait::async_trait]
impl crate::Package for AvrFramework {
    async fn ensure_installed(&self) -> fbuild_core::Result<PathBuf> {
        if self.is_installed() {
            let root = self.resolved_dir();
            // Still ensure API is present (may have been cached without it)
            if self.needs_arduino_api {
                let core_dir = self.get_core_dir(&self.core_name);
                super::arduino_api::ensure_arduino_api(&core_dir).await?;
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

        let install_path = self.base.staged_install(validate_fn).await?;

        let root = find_framework_root(&install_path);

        // Fetch ArduinoCore-API if needed (e.g. ArduinoCore-megaavr)
        if self.needs_arduino_api {
            let core_dir_name = self.core_dir_override.as_deref().unwrap_or(&self.core_name);
            let core_dir = root.join("cores").join(core_dir_name);
            super::arduino_api::ensure_arduino_api(&core_dir).await?;
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

    /// `core = arduino` must use PlatformIO's repacked `framework-arduino-avr`
    /// (not upstream ArduinoCore-avr), which ships the extra variants that
    /// PlatformIO board JSONs reference (e.g. `microduino_plus` for the
    /// Microduino Core+ 1284p16m/1284p8m/644pa8m/644pa16m boards).
    #[test]
    fn arduino_core_uses_platformio_framework_arduino_avr() {
        let entry = lookup_entry("arduino").unwrap();
        assert_eq!(entry.version, "5.4.0");
        assert_eq!(
            framework_url(&entry),
            "https://dl.registry.platformio.org/download/platformio/tool/framework-arduino-avr/5.4.0/framework-arduino-avr-5.4.0.tar.gz"
        );
        assert_eq!(
            entry.checksum.as_deref(),
            Some("bf85bcca114bad389fec51fecbf9b66821a233b366bd3f85cdb1cdcba6a28659")
        );
        assert_eq!(entry.validation_path, "cores/arduino/main.cpp");
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

    #[test]
    fn published_alternate_core_defaults_have_verified_archives() {
        for (core, version) in [
            ("MajorCore", "3.1.0"),
            ("MegaCore", "3.1.0"),
            ("MicroCore", "2.5.2"),
            ("MightyCore", "3.1.0"),
            ("dxcore", "1.6.2"),
        ] {
            let entry = lookup_entry(core).unwrap();
            assert_eq!(entry.version, version, "{core}");
            assert!(framework_url(&entry).starts_with("https://dl.registry.platformio.org/"));
            assert_eq!(entry.checksum.as_ref().map(String::len), Some(64));
        }
    }

    #[test]
    fn test_digispark_dtiny_registered() {
        let registry = load_registry();
        assert!(
            registry.contains_key("dtiny"),
            "dtiny must be registered for Digistump/Digispark boards"
        );
    }

    #[test]
    fn test_digispark_dtiny_lookup() {
        let entry = lookup_entry("dtiny").unwrap();
        assert!(entry.github.contains("DigistumpArduino"));
        assert_eq!(entry.name, "digistump-avr-core");
        assert_eq!(entry.core_dir.as_deref(), Some("tiny"));
    }

    #[test]
    fn test_digispark_pro_lookup() {
        let entry = lookup_entry("pro").unwrap();
        assert!(entry.github.contains("DigistumpArduino"));
        assert_eq!(entry.name, "digistump-avr-core");
        assert_eq!(entry.core_dir.as_deref(), Some("pro"));
    }

    /// MicroCore is used by the ATtiny13 board family (FastLED/fbuild#389).
    #[test]
    fn test_microcore_framework_registered() {
        let entry = lookup_entry("MicroCore").unwrap();
        assert_eq!(entry.validation_path, "cores/MicroCore/Arduino.h");
        assert!(entry.checksum.is_some());
    }

    #[test]
    fn platformio_minicore_uses_repacked_core_directory() {
        let project = tempfile::tempdir().unwrap();
        let registry = AvrFramework::for_core_with_override(
            "MiniCore",
            project.path(),
            fbuild_config::PackageOverride {
                url: "https://dl.registry.platformio.org/download/platformio/tool/framework-arduino-avr-minicore/3.1.2/framework-arduino-avr-minicore-3.1.2.tar.gz".to_string(),
                version: "3.1.2".to_string(),
                checksum: None,
            },
        )
        .unwrap();
        assert_eq!(registry.validation_path, "cores/MiniCore/Arduino.h");
        assert!(
            registry
                .get_core_dir("MiniCore")
                .ends_with("cores/MiniCore")
        );

        let github = AvrFramework::for_core("MiniCore", project.path()).unwrap();
        assert_eq!(github.validation_path, "cores/MCUdude_corefiles/Arduino.h");
        assert!(
            github
                .get_core_dir("MiniCore")
                .ends_with("cores/MCUdude_corefiles")
        );
    }

    /// DxCore is used by the AVR-Dx family (FastLED/fbuild#389).
    #[test]
    fn test_dxcore_framework_registered() {
        let entry = lookup_entry("dxcore").unwrap();
        assert_eq!(entry.validation_path, "cores/dxcore/Arduino.h");
        assert!(entry.checksum.is_some());
    }
}
