//! ESP32 pioarduino platform package.
//!
//! Downloads `platform-espressif32.zip` from pioarduino, which contains
//! `platform.json` with metadata URLs for toolchains and frameworks.

use std::path::{Path, PathBuf};

use fbuild_core::{FbuildError, Result};

use crate::{CacheSubdir, PackageBase, PackageInfo};

/// Platform release URL (stable channel).
const PLATFORM_URL: &str = "https://github.com/pioarduino/platform-espressif32/releases/download/stable/platform-espressif32.zip";

/// Platform version label.
const PLATFORM_VERSION: &str = "stable";

/// ESP32 platform package from pioarduino.
///
/// Provides access to `platform.json` for resolving toolchain metadata URLs.
pub struct Esp32Platform {
    base: PackageBase,
    install_dir: Option<PathBuf>,
}

impl Esp32Platform {
    pub fn new(project_dir: &Path) -> Self {
        Self {
            base: PackageBase::new(
                "platform-espressif32",
                PLATFORM_VERSION,
                PLATFORM_URL,
                PLATFORM_URL,
                None, // No checksum for stable release (content changes)
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
                "platform-espressif32",
                PLATFORM_VERSION,
                PLATFORM_URL,
                PLATFORM_URL,
                None,
                CacheSubdir::Platforms,
                project_dir,
                cache_root,
            ),
            install_dir: None,
        }
    }

    /// Get the resolved install directory.
    fn resolved_dir(&self) -> PathBuf {
        self.install_dir
            .clone()
            .unwrap_or_else(|| find_platform_root(&self.base.install_path()))
    }

    /// Get the toolchain metadata URL from platform.json.
    ///
    /// For RISC-V MCUs, returns the URL for `toolchain-riscv32-esp`.
    /// For Xtensa MCUs, returns the URL for `toolchain-xtensa-esp-elf`.
    pub fn get_toolchain_metadata_url(&self, is_riscv: bool) -> Result<String> {
        let package_name = if is_riscv {
            "toolchain-riscv32-esp"
        } else {
            "toolchain-xtensa-esp-elf"
        };
        self.get_package_url(package_name)
    }

    /// Read and parse the `packages` section of `platform.json`.
    ///
    /// Shared between [`Self::get_package_url`] and
    /// [`Self::enumerate_packages`] so the read/parse/lookup logic stays in
    /// one place and the error messages stay consistent.
    fn read_packages_section(&self) -> Result<serde_json::Map<String, serde_json::Value>> {
        let platform_json_path = self.resolved_dir().join("platform.json");

        let content = std::fs::read_to_string(&platform_json_path).map_err(|e| {
            FbuildError::PackageError(format!(
                "failed to read platform.json at {}: {}",
                platform_json_path.display(),
                e
            ))
        })?;

        let data: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
            FbuildError::PackageError(format!("failed to parse platform.json: {}", e))
        })?;

        data.get("packages")
            .and_then(|p| p.as_object())
            .cloned()
            .ok_or_else(|| {
                FbuildError::PackageError("platform.json has no `packages` section".to_string())
            })
    }

    /// Get a package URL from platform.json by package name.
    ///
    /// The `packages` section of platform.json maps package names to objects
    /// with a `version` field that contains the metadata URL.
    pub fn get_package_url(&self, package_name: &str) -> Result<String> {
        let packages = self.read_packages_section()?;
        packages
            .get(package_name)
            .and_then(|pkg| pkg.get("version"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| {
                FbuildError::PackageError(format!(
                    "package '{}' not found in platform.json",
                    package_name
                ))
            })
    }

    /// Enumerate every package listed in `platform.json`'s `packages` section.
    ///
    /// Returns `(name, version_url)` pairs sorted by name. The orchestrator
    /// uses this to surface helper packages (e.g. `toolchain-riscv32-esp` for
    /// ULP code on ESP32-S3) that are listed alongside the MCU-primary
    /// toolchain. See fbuild#401.
    pub fn enumerate_packages(&self) -> Result<Vec<(String, String)>> {
        let packages = self.read_packages_section()?;
        let mut entries: Vec<(String, String)> = packages
            .iter()
            .filter_map(|(name, value)| {
                value
                    .get("version")
                    .and_then(|v| v.as_str())
                    .map(|url| (name.clone(), url.to_string()))
            })
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(entries)
    }

    /// Validate the platform installation.
    fn validate_install(install_dir: &Path) -> Result<()> {
        let root = find_platform_root(install_dir);
        let platform_json = root.join("platform.json");

        if !platform_json.exists() {
            return Err(FbuildError::PackageError(format!(
                "platform.json not found in {}",
                root.display()
            )));
        }

        let boards_dir = root.join("boards");
        if !boards_dir.exists() {
            return Err(FbuildError::PackageError(format!(
                "boards directory not found in {}",
                root.display()
            )));
        }

        Ok(())
    }
}

#[async_trait::async_trait]
impl crate::Package for Esp32Platform {
    async fn ensure_installed(&self) -> Result<PathBuf> {
        if self.is_installed() {
            return Ok(self.resolved_dir());
        }

        let install_path = self.base.staged_install(Self::validate_install).await?;
        Ok(find_platform_root(&install_path))
    }

    fn is_installed(&self) -> bool {
        if !self.base.is_cached() {
            return false;
        }
        let root = find_platform_root(&self.base.install_path());
        root.join("platform.json").exists()
    }

    fn get_info(&self) -> PackageInfo {
        self.base.get_info()
    }
}

/// Find the actual platform root directory (handles nested extraction).
fn find_platform_root(install_dir: &Path) -> PathBuf {
    if install_dir.join("platform.json").exists() {
        return install_dir.to_path_buf();
    }

    // Check one level deep for platform.json or platform-* directories
    if let Ok(entries) = std::fs::read_dir(install_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && path.join("platform.json").exists() {
                return path;
            }
        }
    }

    install_dir.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Package;

    #[test]
    fn test_platform_url() {
        assert!(PLATFORM_URL.contains("pioarduino"));
        assert!(PLATFORM_URL.contains("platform-espressif32"));
    }

    #[test]
    fn test_find_platform_root_direct() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("platform.json"), "{}").unwrap();
        std::fs::create_dir_all(tmp.path().join("boards")).unwrap();
        assert_eq!(find_platform_root(tmp.path()), tmp.path().to_path_buf());
    }

    #[test]
    fn test_find_platform_root_nested() {
        let tmp = tempfile::TempDir::new().unwrap();
        let nested = tmp.path().join("platform-espressif32");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("platform.json"), "{}").unwrap();
        assert_eq!(find_platform_root(tmp.path()), nested);
    }

    #[test]
    fn test_esp32_platform_not_installed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let platform = Esp32Platform::with_cache_root(tmp.path(), &tmp.path().join("cache"));
        assert!(!platform.is_installed());
    }

    #[test]
    fn test_validate_install() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("platform.json"), "{}").unwrap();
        std::fs::create_dir_all(tmp.path().join("boards")).unwrap();
        assert!(Esp32Platform::validate_install(tmp.path()).is_ok());
    }

    #[test]
    fn test_validate_install_missing_json() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(Esp32Platform::validate_install(tmp.path()).is_err());
    }

    /// A pruned slice of pioarduino `platform-espressif32@54.03.20`'s
    /// `platform.json`, capturing the packages section that an ESP32-S3 build
    /// needs to resolve. Tracks fbuild#401: both the MCU-primary Xtensa
    /// toolchain AND the RISC-V helper toolchain (used for ULP coprocessor
    /// code) must be discoverable.
    const PIOARDUINO_54_03_20_PACKAGES_FRAGMENT: &str = r#"{
      "packages": {
        "framework-arduinoespressif32": {
          "type": "framework",
          "version": "https://github.com/pioarduino/esp32-arduino-libs/releases/download/54.03.20/framework-arduinoespressif32-54.03.20.zip"
        },
        "framework-arduinoespressif32-libs": {
          "type": "framework",
          "version": "https://github.com/pioarduino/esp32-arduino-libs/releases/download/idf-release_v5.5-1f31a92e/esp32-arduino-libs-idf-release_v5.5-1f31a92e.zip"
        },
        "toolchain-xtensa-esp-elf": {
          "type": "toolchain",
          "version": "https://github.com/espressif/crosstool-NG/releases/download/esp-14.2.0_20241119/xtensa-esp-elf-14.2.0_20241119-x86_64-w64-mingw32.zip"
        },
        "toolchain-riscv32-esp": {
          "type": "toolchain",
          "version": "https://github.com/espressif/crosstool-NG/releases/download/esp-14.2.0_20241119/riscv32-esp-elf-14.2.0_20241119-x86_64-w64-mingw32.zip"
        },
        "tool-esptoolpy": {
          "type": "uploader",
          "version": "https://github.com/tasmota/esptool/releases/download/v5.1.0/esptool-v5.1.0-windows-amd64.zip"
        }
      }
    }"#;

    fn write_platform_json(dir: &Path, body: &str) {
        std::fs::write(dir.join("platform.json"), body).unwrap();
        std::fs::create_dir_all(dir.join("boards")).unwrap();
    }

    fn platform_with_install_dir(install_dir: &Path) -> Esp32Platform {
        let mut p = Esp32Platform::new(install_dir);
        p.install_dir = Some(install_dir.to_path_buf());
        p
    }

    #[test]
    fn test_get_toolchain_metadata_url_xtensa_pioarduino_54_03_20() {
        // ESP32-S3 build: primary toolchain is Xtensa.
        let tmp = tempfile::TempDir::new().unwrap();
        write_platform_json(tmp.path(), PIOARDUINO_54_03_20_PACKAGES_FRAGMENT);
        let p = platform_with_install_dir(tmp.path());
        let url = p.get_toolchain_metadata_url(false).unwrap();
        assert!(
            url.contains("xtensa-esp-elf"),
            "expected Xtensa URL, got {url}"
        );
    }

    #[test]
    fn test_get_riscv_helper_toolchain_for_esp32s3_pioarduino_54_03_20() {
        // fbuild#401: even on Xtensa MCUs (ESP32-S3 has Xtensa cores + a
        // RISC-V ULP coprocessor), platform.json lists the RISC-V toolchain.
        // The orchestrator must be able to resolve it on demand so ULP code
        // can be compiled.
        let tmp = tempfile::TempDir::new().unwrap();
        write_platform_json(tmp.path(), PIOARDUINO_54_03_20_PACKAGES_FRAGMENT);
        let p = platform_with_install_dir(tmp.path());
        let url = p.get_package_url("toolchain-riscv32-esp").unwrap();
        assert!(
            url.contains("riscv32-esp-elf"),
            "expected RISC-V helper URL, got {url}"
        );
    }

    #[test]
    fn test_enumerate_packages_returns_all_entries() {
        let tmp = tempfile::TempDir::new().unwrap();
        write_platform_json(tmp.path(), PIOARDUINO_54_03_20_PACKAGES_FRAGMENT);
        let p = platform_with_install_dir(tmp.path());

        let entries = p.enumerate_packages().unwrap();
        let names: Vec<&str> = entries.iter().map(|(n, _)| n.as_str()).collect();
        assert!(
            names.contains(&"framework-arduinoespressif32"),
            "missing framework, got {names:?}"
        );
        assert!(
            names.contains(&"framework-arduinoespressif32-libs"),
            "missing framework-libs, got {names:?}"
        );
        assert!(
            names.contains(&"toolchain-xtensa-esp-elf"),
            "missing Xtensa toolchain, got {names:?}"
        );
        assert!(
            names.contains(&"toolchain-riscv32-esp"),
            "missing RISC-V helper toolchain, got {names:?}"
        );
        assert!(
            names.contains(&"tool-esptoolpy"),
            "missing esptoolpy, got {names:?}"
        );
        // Sorted alphabetically for deterministic iteration order.
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "enumerate_packages must return sorted list");
    }

    #[test]
    fn test_enumerate_packages_errors_when_section_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        write_platform_json(tmp.path(), r#"{"name": "espressif32"}"#);
        let p = platform_with_install_dir(tmp.path());
        let err = p.enumerate_packages().unwrap_err();
        assert!(
            format!("{err}").contains("no `packages` section"),
            "wrong error: {err}"
        );
    }
}
