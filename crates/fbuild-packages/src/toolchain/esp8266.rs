//! ESP8266 toolchain package — Xtensa LX106 GCC from earlephilhower.
//!
//! Downloads and manages the `xtensa-lx106-elf-gcc` toolchain used by
//! the Arduino ESP8266 core.  Binaries come from the `esp-quick-toolchain`
//! GitHub releases.

use std::path::{Path, PathBuf};

use crate::{CacheSubdir, PackageBase, PackageInfo, Toolchain};

/// Toolchain version (matches Arduino ESP8266 core 3.1.2 / espressif8266@4.2.1).
const ESP8266_TOOLCHAIN_VERSION: &str = "3.2.0-gcc10.3";

/// Binary prefix for all tools.
const PREFIX: &str = "xtensa-lx106-elf-";

/// ESP8266 Xtensa-LX106 toolchain manager.
pub struct Esp8266Toolchain {
    base: PackageBase,
    install_dir: Option<PathBuf>,
}

impl Esp8266Toolchain {
    pub fn new(project_dir: &Path) -> Self {
        let (url, checksum) = platform_package();
        Self {
            base: PackageBase::new(
                "esp8266-xtensa-gcc",
                ESP8266_TOOLCHAIN_VERSION,
                &url,
                &url,
                checksum.as_deref(),
                CacheSubdir::Toolchains,
                project_dir,
            ),
            install_dir: None,
        }
    }

    #[cfg(test)]
    fn with_cache_root(project_dir: &Path, cache_root: &Path) -> Self {
        let (url, checksum) = platform_package();
        Self {
            base: PackageBase::with_cache_root(
                "esp8266-xtensa-gcc",
                ESP8266_TOOLCHAIN_VERSION,
                &url,
                &url,
                checksum.as_deref(),
                CacheSubdir::Toolchains,
                project_dir,
                cache_root,
            ),
            install_dir: None,
        }
    }

    fn resolved_dir(&self) -> PathBuf {
        self.install_dir
            .clone()
            .unwrap_or_else(|| find_bin_root(&self.base.install_path()))
    }

    fn validate(install_dir: &Path) -> fbuild_core::Result<()> {
        let root = find_bin_root(install_dir);
        let bin_dir = root.join("bin");

        if !bin_dir.exists() {
            return Err(fbuild_core::FbuildError::PackageError(format!(
                "ESP8266 toolchain bin directory not found at {}",
                bin_dir.display()
            )));
        }

        let required_tools = ["gcc", "g++", "ar", "objcopy", "size"];
        for tool in &required_tools {
            let tool_path = tool_binary(&bin_dir, &format!("{PREFIX}{tool}"));
            if !tool_path.exists() {
                return Err(fbuild_core::FbuildError::PackageError(format!(
                    "required tool {PREFIX}{tool} not found at {}",
                    tool_path.display()
                )));
            }
        }

        Ok(())
    }
}

impl crate::Package for Esp8266Toolchain {
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

        Ok(find_bin_root(&install_path))
    }

    fn is_installed(&self) -> bool {
        if !self.base.is_cached() {
            return false;
        }
        let root = find_bin_root(&self.base.install_path());
        root.join("bin")
            .join(tool_name(&format!("{PREFIX}gcc")))
            .exists()
    }

    fn get_info(&self) -> PackageInfo {
        self.base.get_info()
    }
}

impl Toolchain for Esp8266Toolchain {
    fn get_gcc_path(&self) -> PathBuf {
        tool_binary(&self.resolved_dir().join("bin"), &format!("{PREFIX}gcc"))
    }

    fn get_gxx_path(&self) -> PathBuf {
        tool_binary(&self.resolved_dir().join("bin"), &format!("{PREFIX}g++"))
    }

    fn get_ar_path(&self) -> PathBuf {
        tool_binary(&self.resolved_dir().join("bin"), &format!("{PREFIX}ar"))
    }

    fn get_objcopy_path(&self) -> PathBuf {
        tool_binary(
            &self.resolved_dir().join("bin"),
            &format!("{PREFIX}objcopy"),
        )
    }

    fn get_size_path(&self) -> PathBuf {
        tool_binary(&self.resolved_dir().join("bin"), &format!("{PREFIX}size"))
    }

    fn get_bin_dir(&self) -> PathBuf {
        self.resolved_dir().join("bin")
    }
}

// ── Platform-specific download URLs ────────────────────────────────────

struct PlatformPackage {
    filename_suffix: &'static str,
    checksum: Option<&'static str>,
}

fn all_platform_packages() -> [(&'static str, PlatformPackage); 5] {
    [
        (
            "windows",
            PlatformPackage {
                filename_suffix: "x86_64-w64-mingw32.xtensa-lx106-elf-c791b74.230224.zip",
                checksum: None,
            },
        ),
        (
            // No aarch64-apple-darwin build exists; use x86_64 under Rosetta 2.
            "macos-arm64",
            PlatformPackage {
                filename_suffix: "x86_64-apple-darwin14.xtensa-lx106-elf-c791b74.230224.tar.gz",
                checksum: None,
            },
        ),
        (
            "macos-x86_64",
            PlatformPackage {
                filename_suffix: "x86_64-apple-darwin14.xtensa-lx106-elf-c791b74.230224.tar.gz",
                checksum: None,
            },
        ),
        (
            "linux-aarch64",
            PlatformPackage {
                filename_suffix: "aarch64-linux-gnu.xtensa-lx106-elf-c791b74.230224.tar.gz",
                checksum: None,
            },
        ),
        (
            "linux-x86_64",
            PlatformPackage {
                filename_suffix: "x86_64-linux-gnu.xtensa-lx106-elf-c791b74.230224.tar.gz",
                checksum: None,
            },
        ),
    ]
}

fn platform_package() -> (String, Option<String>) {
    let key = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            "macos-arm64"
        } else {
            "macos-x86_64"
        }
    } else if cfg!(target_arch = "aarch64") {
        "linux-aarch64"
    } else {
        "linux-x86_64"
    };

    let packages = all_platform_packages();
    let (_, pkg) = packages
        .iter()
        .find(|(k, _)| *k == key)
        .expect("no ESP8266 toolchain package for current platform");

    let url = format!(
        "https://github.com/earlephilhower/esp-quick-toolchain/releases/download/{}/{}",
        ESP8266_TOOLCHAIN_VERSION, pkg.filename_suffix,
    );

    (url, pkg.checksum.map(|s| s.to_string()))
}

// ── Helpers ────────────────────────────────────────────────────────────

fn find_bin_root(install_dir: &Path) -> PathBuf {
    if install_dir.join("bin").exists() {
        return install_dir.to_path_buf();
    }

    if let Ok(entries) = std::fs::read_dir(install_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && path.join("bin").exists() {
                return path;
            }
        }
    }

    install_dir.to_path_buf()
}

fn tool_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

fn tool_binary(bin_dir: &Path, name: &str) -> PathBuf {
    bin_dir.join(tool_name(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Package;

    #[test]
    fn test_platform_package_returns_url() {
        let (url, _checksum) = platform_package();
        assert!(url.contains("xtensa-lx106-elf"));
        assert!(url.contains("earlephilhower"));
    }

    #[test]
    fn test_tool_name_platform() {
        let name = tool_name("xtensa-lx106-elf-gcc");
        if cfg!(windows) {
            assert_eq!(name, "xtensa-lx106-elf-gcc.exe");
        } else {
            assert_eq!(name, "xtensa-lx106-elf-gcc");
        }
    }

    #[test]
    fn test_find_bin_root_direct() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("bin")).unwrap();
        assert_eq!(find_bin_root(tmp.path()), tmp.path().to_path_buf());
    }

    #[test]
    fn test_find_bin_root_nested() {
        let tmp = tempfile::TempDir::new().unwrap();
        let nested = tmp.path().join("xtensa-lx106-elf");
        std::fs::create_dir_all(nested.join("bin")).unwrap();
        assert_eq!(find_bin_root(tmp.path()), nested);
    }

    #[test]
    fn test_esp8266_toolchain_not_installed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tc = Esp8266Toolchain::with_cache_root(tmp.path(), &tmp.path().join("cache"));
        assert!(!tc.is_installed());
    }

    #[test]
    fn test_esp8266_toolchain_tools() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tc = Esp8266Toolchain::new(tmp.path());
        let tools = tc.get_all_tools();
        assert!(tools.contains_key("gcc"));
        assert!(tools.contains_key("g++"));
        assert!(tools.contains_key("ar"));
        assert!(tools.contains_key("objcopy"));
        assert!(tools.contains_key("size"));
        let gcc = tools.get("gcc").unwrap().to_string_lossy().to_string();
        assert!(gcc.contains("xtensa-lx106-elf-gcc"));
    }

    #[test]
    fn test_all_checksums_are_valid_or_none() {
        for (platform, pkg) in &all_platform_packages() {
            if let Some(hash) = pkg.checksum {
                assert_eq!(
                    hash.len(),
                    64,
                    "checksum for {platform} has wrong length ({}): {hash}",
                    hash.len(),
                );
                assert!(
                    hash.chars().all(|c| c.is_ascii_hexdigit()),
                    "checksum for {platform} contains non-hex characters: {hash}",
                );
            }
        }
    }
}
