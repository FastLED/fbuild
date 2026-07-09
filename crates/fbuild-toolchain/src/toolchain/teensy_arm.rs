//! Teensy ARM GCC toolchain package.
//!
//! Teensyduino 1.60 is compatible with PlatformIO's Teensy-pinned
//! `toolchain-gccarmnoneeabi-teensy@1.110301.0` package, which contains
//! arm-none-eabi GCC 11.3.1.

use std::path::{Path, PathBuf};

use crate::{CacheSubdir, PackageBase, PackageInfo, Toolchain};

const TEENSY_ARM_TOOLCHAIN_NAME: &str = "toolchain-gccarmnoneeabi-teensy";
const TEENSY_ARM_TOOLCHAIN_VERSION: &str = "1.110301.0";
const TEENSY_ARM_TOOLCHAIN_BASE_URL: &str =
    "https://dl.registry.platformio.org/download/platformio/tool/toolchain-gccarmnoneeabi-teensy/1.110301.0";

/// Teensy-compatible ARM GCC toolchain manager.
pub struct TeensyArmToolchain {
    base: PackageBase,
    install_dir: Option<PathBuf>,
}

impl TeensyArmToolchain {
    pub fn new(project_dir: &Path) -> Self {
        let package = platform_package();
        Self {
            base: PackageBase::new(
                TEENSY_ARM_TOOLCHAIN_NAME,
                TEENSY_ARM_TOOLCHAIN_VERSION,
                &package.url(),
                TEENSY_ARM_TOOLCHAIN_NAME,
                Some(package.checksum),
                CacheSubdir::Toolchains,
                project_dir,
            ),
            install_dir: None,
        }
    }

    #[cfg(test)]
    fn with_cache_root(project_dir: &Path, cache_root: &Path) -> Self {
        let package = platform_package();
        Self {
            base: PackageBase::with_cache_root(
                TEENSY_ARM_TOOLCHAIN_NAME,
                TEENSY_ARM_TOOLCHAIN_VERSION,
                &package.url(),
                TEENSY_ARM_TOOLCHAIN_NAME,
                Some(package.checksum),
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
                "Teensy ARM GCC bin directory not found at {}",
                bin_dir.display()
            )));
        }

        for tool in REQUIRED_TOOLS {
            let tool_path = tool_binary(&bin_dir, tool);
            if !tool_path.exists() {
                return Err(fbuild_core::FbuildError::PackageError(format!(
                    "required Teensy ARM GCC tool {} not found at {}",
                    tool,
                    tool_path.display()
                )));
            }
        }

        Ok(())
    }
}

#[async_trait::async_trait]
impl crate::Package for TeensyArmToolchain {
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

impl Toolchain for TeensyArmToolchain {
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

const REQUIRED_TOOLS: &[&str] = &[
    "arm-none-eabi-gcc",
    "arm-none-eabi-g++",
    "arm-none-eabi-ar",
    "arm-none-eabi-objcopy",
    "arm-none-eabi-size",
];

#[derive(Debug, Clone, Copy)]
struct TeensyArmPlatformPackage {
    platform: &'static str,
    filename: &'static str,
    checksum: &'static str,
}

fn all_platform_packages() -> [TeensyArmPlatformPackage; 6] {
    [
        TeensyArmPlatformPackage {
            platform: "windows",
            filename: "toolchain-gccarmnoneeabi-teensy-windows-1.110301.0.tar.gz",
            checksum: "cfa2479b0eb96e4081ad458891331edc2d0fa8f19bcd3ef54645d886ad1d90b1",
        },
        TeensyArmPlatformPackage {
            platform: "macos",
            filename: "toolchain-gccarmnoneeabi-teensy-darwin_x86_64-1.110301.0.tar.gz",
            checksum: "8f88880ea0a23b01c5baae3e3c343b94a84cea51bce957f4b551834628e6f2de",
        },
        TeensyArmPlatformPackage {
            platform: "linux-aarch64",
            filename: "toolchain-gccarmnoneeabi-teensy-linux_aarch64-1.110301.0.tar.gz",
            checksum: "8c626d4cf321b85c7f602c76a690151eac650bb3b1bd9c47424bcc4775bd4126",
        },
        TeensyArmPlatformPackage {
            platform: "linux-arm",
            filename: "toolchain-gccarmnoneeabi-teensy-linux_armv6l-1.110301.0.tar.gz",
            checksum: "006da609b85a2dff842ec8b443b9ecae8c63cf7fc8e799df5481eff909a9c328",
        },
        TeensyArmPlatformPackage {
            platform: "linux-i686",
            filename: "toolchain-gccarmnoneeabi-teensy-linux_i686-1.110301.0.tar.gz",
            checksum: "392330e8fdca75bc63f261ceed304683367df66dc0dd0cdb9ae501e1addfdb76",
        },
        TeensyArmPlatformPackage {
            platform: "linux-x86_64",
            filename: "toolchain-gccarmnoneeabi-teensy-linux_x86_64-1.110301.0.tar.gz",
            checksum: "958891c6cc68862bd07af7914291173721d1c5a0fcbbb9b05e8e17564dd652aa",
        },
    ]
}

fn platform_package() -> TeensyArmPlatformPackage {
    let key = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_arch = "aarch64") {
        "linux-aarch64"
    } else if cfg!(target_arch = "arm") {
        "linux-arm"
    } else if cfg!(target_arch = "x86") {
        "linux-i686"
    } else {
        "linux-x86_64"
    };

    all_platform_packages()
        .into_iter()
        .find(|package| package.platform == key)
        .expect("no Teensy ARM GCC package for current platform")
}

impl TeensyArmPlatformPackage {
    fn url(&self) -> String {
        format!("{}/{}", TEENSY_ARM_TOOLCHAIN_BASE_URL, self.filename)
    }
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
        format!("{}.exe", name)
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
    use crate::{Package, Toolchain};
    use std::collections::HashMap;

    #[test]
    fn test_platform_package_returns_platformio_url() {
        let package = platform_package();
        let url = package.url();
        assert!(url.starts_with("https://dl.registry.platformio.org"));
        assert!(url.contains(TEENSY_ARM_TOOLCHAIN_NAME));
        assert!(url.contains(TEENSY_ARM_TOOLCHAIN_VERSION));
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
        let nested = tmp.path().join("gcc-arm-none-eabi-11.3.1");
        std::fs::create_dir_all(nested.join("bin")).unwrap();
        assert_eq!(find_bin_root(tmp.path()), nested);
    }

    #[test]
    fn test_teensy_arm_toolchain_get_tools() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tc = TeensyArmToolchain::new(tmp.path());
        let tools: HashMap<String, PathBuf> = tc.get_all_tools();
        assert!(tools.contains_key("gcc"));
        assert!(tools.contains_key("g++"));
        assert!(tools.contains_key("ar"));
        assert!(tools.contains_key("objcopy"));
        assert!(tools.contains_key("size"));
    }

    #[test]
    fn test_teensy_arm_toolchain_not_installed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tc = TeensyArmToolchain::with_cache_root(tmp.path(), &tmp.path().join("cache"));
        assert!(!tc.is_installed());
    }

    #[test]
    fn test_package_info_uses_platformio_package_version() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tc = TeensyArmToolchain::with_cache_root(tmp.path(), &tmp.path().join("cache"));
        let info = tc.get_info();
        assert_eq!(info.name, TEENSY_ARM_TOOLCHAIN_NAME);
        assert_eq!(info.version, TEENSY_ARM_TOOLCHAIN_VERSION);
        assert!(info.url.contains(TEENSY_ARM_TOOLCHAIN_VERSION));
    }

    #[test]
    fn test_all_checksums_are_valid_sha256() {
        for package in all_platform_packages() {
            assert_eq!(
                package.checksum.len(),
                64,
                "checksum for {} has wrong length",
                package.platform,
            );
            assert!(
                package.checksum.chars().all(|c| c.is_ascii_hexdigit()),
                "checksum for {} contains non-hex characters",
                package.platform,
            );
        }
    }
}
