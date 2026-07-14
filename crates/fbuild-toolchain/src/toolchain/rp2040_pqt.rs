//! RP2040/RP2350 ARM GCC toolchain package from earlephilhower's pico-quick-toolchain.
//!
//! This matches the toolchain version bundled by arduino-pico 4.5.3, which is
//! newer and ABI-distinct from the generic Arm 15.x toolchain used by other
//! ARM platforms in fbuild.

use std::path::{Path, PathBuf};

use crate::{CacheSubdir, PackageBase, PackageInfo, Toolchain};

/// Toolchain version from arduino-pico 4.5.3's package index.
const RP2040_PQT_VERSION: &str = "4.0.1-8ec9d6f";
const RP2040_PQT_BASE_URL: &str =
    "https://github.com/earlephilhower/pico-quick-toolchain/releases/download/4.0.1";

/// RP2040/RP2350 ARM GCC toolchain manager.
pub struct Rp2040PqtToolchain {
    base: PackageBase,
    install_dir: Option<PathBuf>,
}

impl Rp2040PqtToolchain {
    pub fn new(project_dir: &Path) -> Self {
        let (url, checksum) = platform_package();
        Self {
            base: PackageBase::new(
                "rp2040-pqt-gcc",
                RP2040_PQT_VERSION,
                &url,
                RP2040_PQT_BASE_URL,
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
                "rp2040-pqt-gcc",
                RP2040_PQT_VERSION,
                &url,
                RP2040_PQT_BASE_URL,
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
                "rp2040-pqt-gcc bin directory not found at {}",
                bin_dir.display()
            )));
        }

        let required_tools = [
            "arm-none-eabi-gcc",
            "arm-none-eabi-g++",
            "arm-none-eabi-ar",
            "arm-none-eabi-objcopy",
            "arm-none-eabi-size",
        ];
        for tool in &required_tools {
            let tool_path = tool_binary(&bin_dir, tool);
            if !tool_path.exists() {
                return Err(fbuild_core::FbuildError::PackageError(format!(
                    "required tool {} not found at {}",
                    tool,
                    tool_path.display()
                )));
            }
        }

        Ok(())
    }
}

#[async_trait::async_trait]
impl crate::Package for Rp2040PqtToolchain {
    async fn ensure_installed(&self) -> fbuild_core::Result<PathBuf> {
        if self.is_installed() {
            return Ok(self.resolved_dir());
        }

        let install_path = self.base.staged_install(Self::validate).await?;
        Ok(find_bin_root(&install_path))
    }

    fn is_installed(&self) -> bool {
        if !self.base.is_cached() {
            return false;
        }
        let root = find_bin_root(&self.base.install_path());
        root.join("bin")
            .join(tool_name("arm-none-eabi-gcc"))
            .exists()
    }

    fn get_info(&self) -> PackageInfo {
        self.base.get_info()
    }
}

impl Toolchain for Rp2040PqtToolchain {
    fn get_gcc_path(&self) -> PathBuf {
        tool_binary(&self.resolved_dir().join("bin"), "arm-none-eabi-gcc")
    }

    fn get_gxx_path(&self) -> PathBuf {
        tool_binary(&self.resolved_dir().join("bin"), "arm-none-eabi-g++")
    }

    fn get_ar_path(&self) -> PathBuf {
        tool_binary(&self.resolved_dir().join("bin"), "arm-none-eabi-ar")
    }

    fn get_objcopy_path(&self) -> PathBuf {
        tool_binary(&self.resolved_dir().join("bin"), "arm-none-eabi-objcopy")
    }

    fn get_size_path(&self) -> PathBuf {
        tool_binary(&self.resolved_dir().join("bin"), "arm-none-eabi-size")
    }

    fn get_bin_dir(&self) -> PathBuf {
        self.resolved_dir().join("bin")
    }
}

struct PlatformPackage {
    filename: &'static str,
    checksum: Option<&'static str>,
}

fn all_platform_packages() -> [(&'static str, PlatformPackage); 5] {
    [
        (
            "windows",
            PlatformPackage {
                filename: "x86_64-w64-mingw32.arm-none-eabi-8ec9d6f.240929.zip",
                checksum: Some("a1ac18cde856fa01aafc9985a719f3749abd3588ac6725d1781f02da94b84d54"),
            },
        ),
        (
            "macos-arm64",
            PlatformPackage {
                filename: "aarch64-apple-darwin20.4.arm-none-eabi-8ec9d6f.240929.tar.gz",
                checksum: None,
            },
        ),
        (
            "macos-x86_64",
            PlatformPackage {
                filename: "x86_64-apple-darwin20.4.arm-none-eabi-8ec9d6f.240929.tar.gz",
                checksum: None,
            },
        ),
        (
            "linux-aarch64",
            PlatformPackage {
                filename: "aarch64-linux-gnu.arm-none-eabi-8ec9d6f.240929.tar.gz",
                checksum: None,
            },
        ),
        (
            "linux-x86_64",
            PlatformPackage {
                filename: "x86_64-linux-gnu.arm-none-eabi-8ec9d6f.240929.tar.gz",
                checksum: Some("ae082491cc07d60c014ca928c406aed72c4b1ead4c33076216c77fd2d242f74d"),
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
        .expect("no rp2040 pqt package for current platform");

    (
        format!("{}/{}", RP2040_PQT_BASE_URL, pkg.filename),
        pkg.checksum.map(|s| s.to_string()),
    )
}

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
        assert!(url.contains("4.0.1"));
        assert!(url.contains("arm-none-eabi"));
    }

    #[test]
    fn test_tool_name_platform() {
        let name = tool_name("arm-none-eabi-gcc");
        if cfg!(windows) {
            assert_eq!(name, "arm-none-eabi-gcc.exe");
        } else {
            assert_eq!(name, "arm-none-eabi-gcc");
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
        let nested = tmp.path().join("pqt");
        std::fs::create_dir_all(nested.join("bin")).unwrap();
        assert_eq!(find_bin_root(tmp.path()), nested);
    }

    #[test]
    fn test_rp2040_pqt_not_installed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tc = Rp2040PqtToolchain::with_cache_root(tmp.path(), &tmp.path().join("cache"));
        assert!(!tc.is_installed());
    }

    #[test]
    fn test_rp2040_pqt_tools() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tc = Rp2040PqtToolchain::new(tmp.path());
        let tools = tc.get_all_tools();
        assert!(tools.contains_key("gcc"));
        assert!(tools.contains_key("g++"));
        assert!(tools.contains_key("ar"));
        assert!(tools.contains_key("objcopy"));
        assert!(tools.contains_key("size"));
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
