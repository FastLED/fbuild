//! ESP8266 Arduino framework package.
//!
//! Downloads and manages the Arduino ESP8266 core from GitHub.
//! Layout inside the archive:
//! ```text
//! Arduino-<version>/
//!   cores/esp8266/       # core source files
//!   variants/nodemcu/    # board variant files
//!   libraries/           # built-in libraries (ESP8266WiFi, etc.)
//!   tools/sdk/           # precompiled SDK
//!     include/           # SDK headers
//!     lib/               # precompiled .a files (libmain.a, libphy.a, …)
//!     ld/                # linker scripts (eagle.flash.*.ld)
//! ```

use std::path::{Path, PathBuf};

use crate::{CacheSubdir, Framework, PackageBase, PackageInfo};

/// Framework version matching espressif8266@4.2.1.
const ESP8266_FRAMEWORK_VERSION: &str = "3.1.2";
const ESP8266_FRAMEWORK_URL: &str =
    "https://github.com/esp8266/Arduino/archive/refs/tags/3.1.2.tar.gz";

/// ESP8266 Arduino framework manager.
pub struct Esp8266Framework {
    base: PackageBase,
}

impl Esp8266Framework {
    pub fn new(project_dir: &Path) -> Self {
        Self {
            base: PackageBase::new(
                "esp8266-arduino",
                ESP8266_FRAMEWORK_VERSION,
                ESP8266_FRAMEWORK_URL,
                ESP8266_FRAMEWORK_URL,
                None,
                CacheSubdir::Platforms,
                project_dir,
            ),
        }
    }

    #[cfg(test)]
    fn with_cache_root(project_dir: &Path, cache_root: &Path) -> Self {
        Self {
            base: PackageBase::with_cache_root(
                "esp8266-arduino",
                ESP8266_FRAMEWORK_VERSION,
                ESP8266_FRAMEWORK_URL,
                ESP8266_FRAMEWORK_URL,
                None,
                CacheSubdir::Platforms,
                project_dir,
                cache_root,
            ),
        }
    }

    fn resolved_dir(&self) -> PathBuf {
        find_framework_root(&self.base.install_path())
    }

    // ── SDK paths ───────────────────────────────────────────────────────

    /// SDK precompiled libraries (`tools/sdk/lib/`).
    pub fn get_sdk_lib_dir(&self) -> PathBuf {
        self.resolved_dir().join("tools").join("sdk").join("lib")
    }

    /// NonOS SDK version-specific libraries (`tools/sdk/lib/NONOSDK22x_190703/`, etc.).
    ///
    /// The ESP8266 NonOS SDK ships precompiled `.a` files (libphy, libpp,
    /// libnet80211, etc.) in version-specific subdirectories under `tools/sdk/lib/`.
    /// The version matches the `build.sdk` property from `platform.txt`.
    pub fn get_sdk_nonosdk_lib_dir(&self) -> PathBuf {
        self.get_sdk_nonosdk_lib_dir_for("NONOSDK22x_190703")
    }

    pub fn get_sdk_nonosdk_lib_dir_for(&self, sdk_name: &str) -> PathBuf {
        self.resolved_dir()
            .join("tools")
            .join("sdk")
            .join("lib")
            .join(sdk_name)
    }

    /// SDK libc library directory (`tools/sdk/libc/xtensa-lx106-elf/lib/`).
    ///
    /// Matches `compiler.libc.path` + `/lib` from platform.txt.
    pub fn get_libc_lib_dir(&self) -> PathBuf {
        self.resolved_dir()
            .join("tools")
            .join("sdk")
            .join("libc")
            .join("xtensa-lx106-elf")
            .join("lib")
    }

    /// SDK linker scripts (`tools/sdk/ld/`).
    pub fn get_sdk_ld_dir(&self) -> PathBuf {
        self.resolved_dir().join("tools").join("sdk").join("ld")
    }

    /// Collect SDK include directories.
    ///
    /// The ESP8266 SDK places headers in `tools/sdk/include/` with per-component
    /// subdirectories. We return the top-level dir plus every immediate child dir,
    /// plus `tools/sdk/lwip2/include` (network stack headers like `lwipopts.h`)
    /// and `tools/sdk/libb64/include`.
    pub fn get_sdk_include_dirs(&self) -> Vec<PathBuf> {
        let sdk_base = self.resolved_dir().join("tools").join("sdk");
        let include_base = sdk_base.join("include");
        let mut dirs = Vec::new();
        if include_base.is_dir() {
            dirs.push(include_base.clone());
            if let Ok(entries) = std::fs::read_dir(&include_base) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        dirs.push(path);
                    }
                }
            }
        }
        // lwip2 headers (lwipopts.h, lwip/*.h)
        let lwip2_include = sdk_base.join("lwip2").join("include");
        if lwip2_include.is_dir() {
            dirs.push(lwip2_include);
        }
        // libb64 headers
        let libb64_include = sdk_base.join("libb64").join("include");
        if libb64_include.is_dir() {
            dirs.push(libb64_include);
        }
        dirs
    }

    /// SDK libc include directory (`tools/sdk/libc/xtensa-lx106-elf/include/`).
    ///
    /// Matches `compiler.libc.path` from platform.txt.
    pub fn get_libc_include_dirs(&self) -> Vec<PathBuf> {
        let libc_include = self
            .resolved_dir()
            .join("tools")
            .join("sdk")
            .join("libc")
            .join("xtensa-lx106-elf")
            .join("include");
        if libc_include.is_dir() {
            vec![libc_include]
        } else {
            Vec::new()
        }
    }

    /// Get the core source directory.
    pub fn get_core_dir(&self, core_name: &str) -> PathBuf {
        self.get_cores_dir().join(core_name)
    }

    /// Get the variant directory.
    pub fn get_variant_dir(&self, variant_name: &str) -> PathBuf {
        self.get_variants_dir().join(variant_name)
    }

    /// Get path to boards.txt.
    pub fn get_boards_txt(&self) -> PathBuf {
        self.resolved_dir().join("boards.txt")
    }
}

impl crate::Package for Esp8266Framework {
    fn ensure_installed(&self) -> fbuild_core::Result<PathBuf> {
        if self.is_installed() {
            return Ok(self.resolved_dir());
        }

        let validate_fn = |install_dir: &Path| {
            let root = find_framework_root(install_dir);
            if !root.join("cores").exists() {
                return Err(fbuild_core::FbuildError::PackageError(format!(
                    "ESP8266 framework missing cores/ directory in {}",
                    root.display()
                )));
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

        Ok(find_framework_root(&install_path))
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

impl Framework for Esp8266Framework {
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
/// GitHub archives have a nested structure: `Arduino-3.1.2/cores/`.
fn find_framework_root(install_dir: &Path) -> PathBuf {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Package;

    #[test]
    fn test_esp8266_framework_not_installed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fw = Esp8266Framework::with_cache_root(tmp.path(), &tmp.path().join("cache"));
        assert!(!fw.is_installed());
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
        let nested = tmp.path().join("Arduino-3.1.2");
        std::fs::create_dir_all(nested.join("cores")).unwrap();
        assert_eq!(find_framework_root(tmp.path()), nested);
    }
}
