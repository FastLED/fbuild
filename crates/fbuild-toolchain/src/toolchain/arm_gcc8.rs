//! ARM GCC 9 toolchain package (for Apollo3/mbed-os platforms).
//!
//! The SparkFun Apollo3 core uses mbed-os headers that are incompatible with
//! newer GCC versions (GCC 15+). PlatformIO's platform-apollo3blue specifies
//! `toolchain-gccarmnoneeabi@1.90201.191206` (GCC 9.2.1).
//! Downloads from the PlatformIO registry.

use std::path::{Path, PathBuf};

use crate::{CacheSubdir, PackageBase, PackageInfo, Toolchain};

const ARM_GCC9_VERSION: &str = "9.2.1";

/// ARM GCC 8 toolchain manager (for Apollo3/mbed-os).
pub struct ArmGcc8Toolchain {
    base: PackageBase,
    install_dir: Option<PathBuf>,
}

impl ArmGcc8Toolchain {
    pub fn new(project_dir: &Path) -> Self {
        let url = platform_url();
        Self {
            base: PackageBase::new(
                "arm-gcc8",
                ARM_GCC9_VERSION,
                &url,
                &url,
                None,
                CacheSubdir::Toolchains,
                project_dir,
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
                "arm-gcc8 bin directory not found at {}",
                bin_dir.display()
            )));
        }

        let gcc = tool_binary(&bin_dir, "arm-none-eabi-gcc");
        if !gcc.exists() {
            return Err(fbuild_core::FbuildError::PackageError(format!(
                "arm-none-eabi-gcc not found at {}",
                gcc.display()
            )));
        }

        Ok(())
    }
}

#[async_trait::async_trait]
impl crate::Package for ArmGcc8Toolchain {
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

impl Toolchain for ArmGcc8Toolchain {
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

/// PlatformIO registry base for toolchain-gccarmnoneeabi 1.90201.191206 (GCC 9.2.1).
const PIO_DL_BASE: &str = "https://dl.registry.platformio.org/download/platformio/tool/toolchain-gccarmnoneeabi/1.90201.191206";

/// Get the platform-specific download URL and optional SHA-256 checksum.
fn platform_package() -> (&'static str, Option<&'static str>) {
    if cfg!(target_os = "windows") {
        (
            "toolchain-gccarmnoneeabi-windows_amd64-1.90201.191206.tar.gz",
            Some("31301e144002f2043f60c518b87327dfcfca9f1dc0c1add72322d553d5733f0e"),
        )
    } else if cfg!(target_os = "macos") {
        (
            "toolchain-gccarmnoneeabi-darwin_x86_64-1.90201.191206.tar.gz",
            Some("309fb7cd5c1b12f1ba8daa6f7554cc95c96a81246b6ff4833cbb31436f8f6add"),
        )
    } else {
        (
            "toolchain-gccarmnoneeabi-linux_x86_64-1.90201.191206.tar.gz",
            Some("140fb263798b9dc1950b3831c44d9ab01196f883012b78658b1e002b9035d26c"),
        )
    }
}

fn platform_url() -> String {
    let (filename, _) = platform_package();
    format!("{}/{}", PIO_DL_BASE, filename)
}

/// Find the actual root directory containing bin/ inside an extracted archive.
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

    #[test]
    fn test_platform_url_is_valid() {
        let url = platform_url();
        assert!(url.starts_with("https://dl.registry.platformio.org"));
        assert!(url.contains("toolchain-gccarmnoneeabi"));
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
        let nested = tmp.path().join("gcc-arm-none-eabi-8-2018-q4-major");
        std::fs::create_dir_all(nested.join("bin")).unwrap();
        assert_eq!(find_bin_root(tmp.path()), nested);
    }
}
