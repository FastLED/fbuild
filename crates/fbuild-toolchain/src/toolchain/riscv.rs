//! RISC-V GCC toolchain package (xPack riscv-none-elf-gcc).
//!
//! Downloads and manages the xPack RISC-V GCC toolchain for CH32V and other
//! RISC-V embedded targets. Provides paths to: riscv-none-elf-gcc,
//! riscv-none-elf-g++, riscv-none-elf-ar, riscv-none-elf-objcopy,
//! riscv-none-elf-size.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{CacheSubdir, PackageBase, PackageInfo, Toolchain};

/// xPack RISC-V GCC toolchain version.
const RISCV_GCC_VERSION: &str = "14.2.0-3";
const RISCV_GCC_BASE_URL: &str =
    "https://github.com/xpack-dev-tools/riscv-none-elf-gcc-xpack/releases/download";

/// RISC-V GCC toolchain manager.
pub struct RiscvToolchain {
    base: PackageBase,
    /// Resolved install path (set after ensure_installed)
    install_dir: Option<PathBuf>,
}

impl RiscvToolchain {
    pub fn new(project_dir: &Path) -> Self {
        let (url, checksum) = platform_package();
        Self {
            base: PackageBase::new(
                "riscv-gcc",
                RISCV_GCC_VERSION,
                &url,
                RISCV_GCC_BASE_URL,
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
                "riscv-gcc",
                RISCV_GCC_VERSION,
                &url,
                RISCV_GCC_BASE_URL,
                checksum.as_deref(),
                CacheSubdir::Toolchains,
                project_dir,
                cache_root,
            ),
            install_dir: None,
        }
    }

    /// Get the resolved install directory, or compute it.
    fn resolved_dir(&self) -> PathBuf {
        self.install_dir
            .clone()
            .unwrap_or_else(|| find_bin_root(&self.base.install_path()))
    }

    /// Get all C++ system include directories needed to work around xPack GCC's
    /// broken sysroot resolution on Windows.
    ///
    /// Returns paths that must be passed as `-isystem` (not `-I`), in the
    /// correct search order:
    /// 1. `<root>/riscv-none-elf/include/c++/<ver>/` — base C++ headers
    /// 2. `<root>/riscv-none-elf/include/c++/<ver>/riscv-none-elf/<march>/<mabi>/` — multilib
    /// 3. `<root>/riscv-none-elf/include/c++/<ver>/backward/` — backward compat
    /// 4. `<root>/lib/gcc/riscv-none-elf/<ver>/include/` — GCC builtins
    /// 5. `<root>/lib/gcc/riscv-none-elf/<ver>/include-fixed/` — fixed headers
    /// 6. `<root>/riscv-none-elf/include/` — target system headers
    pub fn get_cxx_system_includes(&self, march: &str, mabi: &str) -> Vec<PathBuf> {
        let root = self.resolved_dir();
        let mut dirs = Vec::new();

        // Ask GCC for the selected multilib instead of deriving its directory from
        // the ISA spelling. The default multilib is reported as `.`.
        let multilib_dir = self
            .get_gcc_multilib_dir(march, mabi)
            .unwrap_or_else(|| PathBuf::from(march.split('_').next().unwrap_or(march)).join(mabi));

        // C++ headers: find the version directory dynamically
        let cxx_base = root.join("riscv-none-elf").join("include").join("c++");
        if let Ok(entries) = std::fs::read_dir(&cxx_base) {
            for entry in entries.flatten() {
                let version_dir = entry.path();
                if version_dir.is_dir() {
                    // 1. Base C++ headers
                    dirs.push(version_dir.clone());
                    // 2. Multilib-specific
                    let multilib = multilib_include_path(&version_dir, &multilib_dir);
                    if multilib.is_dir() {
                        dirs.push(multilib);
                    }
                    // 3. Backward compat
                    let backward = version_dir.join("backward");
                    if backward.is_dir() {
                        dirs.push(backward);
                    }
                    break;
                }
            }
        }

        // GCC internal headers: find the version directory dynamically
        let gcc_lib = root.join("lib").join("gcc").join("riscv-none-elf");
        if let Ok(entries) = std::fs::read_dir(&gcc_lib) {
            for entry in entries.flatten() {
                let ver_dir = entry.path();
                if ver_dir.is_dir() {
                    // 4. GCC builtins
                    let inc = ver_dir.join("include");
                    if inc.is_dir() {
                        dirs.push(inc);
                    }
                    // 5. Fixed headers
                    let inc_fixed = ver_dir.join("include-fixed");
                    if inc_fixed.is_dir() {
                        dirs.push(inc_fixed);
                    }
                    break;
                }
            }
        }

        // 6. Target system headers
        let sys_inc = root.join("riscv-none-elf").join("include");
        if sys_inc.is_dir() {
            dirs.push(sys_inc);
        }

        dirs
    }

    fn get_gcc_multilib_dir(&self, march: &str, mabi: &str) -> Option<PathBuf> {
        // allow-direct-spawn: short synchronous GCC capability probe (-print-multi-directory).
        let output = Command::new(self.get_gcc_path())
            .args([
                format!("-march={march}"),
                format!("-mabi={mabi}"),
                "-print-multi-directory".into(),
            ])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let directory = String::from_utf8(output.stdout).ok()?.trim().to_owned();
        if directory.is_empty() || directory == "." {
            Some(PathBuf::new())
        } else {
            Some(PathBuf::from(directory))
        }
    }

    /// Validate that the toolchain installation has all required files.
    fn validate(install_dir: &Path) -> fbuild_core::Result<()> {
        let root = find_bin_root(install_dir);
        let bin_dir = root.join("bin");

        if !bin_dir.exists() {
            return Err(fbuild_core::FbuildError::PackageError(format!(
                "riscv-gcc bin directory not found at {}",
                bin_dir.display()
            )));
        }

        let required_tools = [
            "riscv-none-elf-gcc",
            "riscv-none-elf-g++",
            "riscv-none-elf-ar",
            "riscv-none-elf-objcopy",
            "riscv-none-elf-size",
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

fn multilib_include_path(version_dir: &Path, multilib_dir: &Path) -> PathBuf {
    version_dir.join("riscv-none-elf").join(multilib_dir)
}

#[async_trait::async_trait]
impl crate::Package for RiscvToolchain {
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
            .join(tool_name("riscv-none-elf-gcc"))
            .exists()
    }

    fn get_info(&self) -> PackageInfo {
        self.base.get_info()
    }
}

impl Toolchain for RiscvToolchain {
    fn get_gcc_path(&self) -> PathBuf {
        tool_binary(&self.resolved_dir().join("bin"), "riscv-none-elf-gcc")
    }

    fn get_gxx_path(&self) -> PathBuf {
        tool_binary(&self.resolved_dir().join("bin"), "riscv-none-elf-g++")
    }

    fn get_ar_path(&self) -> PathBuf {
        tool_binary(&self.resolved_dir().join("bin"), "riscv-none-elf-ar")
    }

    fn get_objcopy_path(&self) -> PathBuf {
        tool_binary(&self.resolved_dir().join("bin"), "riscv-none-elf-objcopy")
    }

    fn get_size_path(&self) -> PathBuf {
        tool_binary(&self.resolved_dir().join("bin"), "riscv-none-elf-size")
    }

    fn get_bin_dir(&self) -> PathBuf {
        self.resolved_dir().join("bin")
    }
}

/// A platform-specific RISC-V toolchain package entry.
struct RiscvPlatformPackage {
    filename: &'static str,
    /// SHA-256 checksum for the immutable upstream release asset.
    checksum: Option<&'static str>,
}

/// All platform variants for the RISC-V toolchain.
fn all_platform_packages() -> [(&'static str, RiscvPlatformPackage); 4] {
    [
        (
            "windows",
            RiscvPlatformPackage {
                filename: "xpack-riscv-none-elf-gcc-14.2.0-3-win32-x64.zip",
                checksum: Some("9bb15efdeca256532c4a83ce6462c7dc1f9cfebe1f1f43d581b2ad7d077209b6"),
            },
        ),
        (
            "macos",
            RiscvPlatformPackage {
                filename: "xpack-riscv-none-elf-gcc-14.2.0-3-darwin-x64.tar.gz",
                checksum: Some("8a6e699f12876152d6386e777675d94529ccc21a57224a69d973f676949a1687"),
            },
        ),
        (
            "linux-aarch64",
            RiscvPlatformPackage {
                filename: "xpack-riscv-none-elf-gcc-14.2.0-3-linux-arm64.tar.gz",
                checksum: Some("0c0551986e30174af55f245e1c3a86c45233fc793bf36586567f266ada6fdd98"),
            },
        ),
        (
            "linux-x86_64",
            RiscvPlatformPackage {
                filename: "xpack-riscv-none-elf-gcc-14.2.0-3-linux-x64.tar.gz",
                checksum: Some("f574415b63f12b09bdd3475223ab492a465d23810646c90c13a4c3b676c83503"),
            },
        ),
    ]
}

/// Get the platform-specific download URL and optional checksum for the current host.
fn platform_package() -> (String, Option<String>) {
    let key = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_arch = "aarch64") {
        "linux-aarch64"
    } else {
        "linux-x86_64"
    };

    let packages = all_platform_packages();
    let (_, pkg) = packages
        .iter()
        .find(|(k, _)| *k == key)
        .expect("no RISC-V package for current platform");

    (
        format!(
            "{}/v{}/{}",
            RISCV_GCC_BASE_URL, RISCV_GCC_VERSION, pkg.filename
        ),
        pkg.checksum.map(|s| s.to_string()),
    )
}

/// Find the actual root directory containing bin/ inside an extracted archive.
///
/// Archives often have a single top-level directory (e.g. `xpack-riscv-none-elf-gcc-.../`).
fn find_bin_root(install_dir: &Path) -> PathBuf {
    if install_dir.join("bin").exists() {
        return install_dir.to_path_buf();
    }

    // Check one level deep for a nested directory with bin/
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

/// Get the tool binary name with .exe extension on Windows.
fn tool_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{}.exe", name)
    } else {
        name.to_string()
    }
}

/// Get the full path to a tool binary.
fn tool_binary(bin_dir: &Path, name: &str) -> PathBuf {
    bin_dir.join(tool_name(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Package;
    use std::collections::HashMap;

    #[test]
    fn test_platform_package_returns_url() {
        let (url, _checksum) = platform_package();
        assert!(url.contains("xpack-dev-tools/riscv-none-elf-gcc-xpack"));
        assert!(url.contains("riscv-none-elf"));
    }

    #[test]
    fn test_tool_name_platform() {
        let name = tool_name("riscv-none-elf-gcc");
        if cfg!(windows) {
            assert_eq!(name, "riscv-none-elf-gcc.exe");
        } else {
            assert_eq!(name, "riscv-none-elf-gcc");
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
        let nested = tmp.path().join("xpack-riscv-none-elf-gcc-14.2.0-3");
        std::fs::create_dir_all(nested.join("bin")).unwrap();
        assert_eq!(find_bin_root(tmp.path()), nested);
    }

    #[test]
    fn test_riscv_toolchain_get_tools() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tc = RiscvToolchain::new(tmp.path());
        let tools: HashMap<String, PathBuf> = tc.get_all_tools();
        assert!(tools.contains_key("gcc"));
        assert!(tools.contains_key("g++"));
        assert!(tools.contains_key("ar"));
        assert!(tools.contains_key("objcopy"));
        assert!(tools.contains_key("size"));
    }

    #[test]
    fn test_riscv_toolchain_not_installed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tc = RiscvToolchain::with_cache_root(tmp.path(), &tmp.path().join("cache"));
        assert!(!tc.is_installed());
    }

    #[test]
    fn test_multilib_include_path_maps_default_directory_to_sysroot() {
        let version_dir = Path::new("toolchain/include/c++/14.2.0");
        assert_eq!(
            multilib_include_path(version_dir, Path::new("")),
            PathBuf::from("toolchain/include/c++/14.2.0/riscv-none-elf")
        );
    }

    #[test]
    fn test_multilib_include_path_preserves_extension_directory() {
        let version_dir = Path::new("toolchain/include/c++/14.2.0");
        assert_eq!(
            multilib_include_path(version_dir, Path::new("rv32imafc_zicsr/ilp32f")),
            PathBuf::from("toolchain/include/c++/14.2.0/riscv-none-elf/rv32imafc_zicsr/ilp32f")
        );
    }

    /// Every platform entry must have a valid URL.
    #[test]
    fn test_all_platform_urls_are_valid() {
        for (platform, pkg) in &all_platform_packages() {
            assert!(
                pkg.filename.contains("14.2.0-3"),
                "filename for {platform} doesn't contain version: {}",
                pkg.filename,
            );
        }
    }
}
