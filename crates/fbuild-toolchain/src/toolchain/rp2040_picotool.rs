//! Managed picotool package from earlephilhower's pico-quick-toolchain.
//!
//! Arduino-Pico uses this exact release family to convert the linked ELF to
//! UF2. Keeping it under fbuild's package manager avoids host `PATH` lookup
//! while preserving Raspberry Pi's official ELF validation and sparse UF2
//! layout.

use std::path::{Path, PathBuf};

use crate::{CacheSubdir, PackageBase, PackageInfo};

const PICOTOOL_VERSION: &str = "4.0.1-8a9af99";
const PICOTOOL_BASE_URL: &str =
    "https://github.com/earlephilhower/pico-quick-toolchain/releases/download/4.0.1";

pub struct Rp2040Picotool {
    base: PackageBase,
    install_dir: Option<PathBuf>,
}

impl Rp2040Picotool {
    pub fn new(project_dir: &Path) -> Self {
        let package = platform_package();
        Self {
            base: PackageBase::new(
                "rp2040-picotool",
                PICOTOOL_VERSION,
                &format!("{PICOTOOL_BASE_URL}/{}", package.filename),
                PICOTOOL_BASE_URL,
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
                "rp2040-picotool",
                PICOTOOL_VERSION,
                &format!("{PICOTOOL_BASE_URL}/{}", package.filename),
                PICOTOOL_BASE_URL,
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
            .unwrap_or_else(|| find_picotool_root(&self.base.install_path()))
    }

    pub fn executable(&self) -> PathBuf {
        self.resolved_dir().join(picotool_name())
    }

    fn validate(install_dir: &Path) -> fbuild_core::Result<()> {
        let executable = find_picotool_root(install_dir).join(picotool_name());
        if !executable.is_file() {
            return Err(fbuild_core::FbuildError::PackageError(format!(
                "managed picotool executable not found at {}; managed picotool assets exist for {} (the windows asset is x86_64 and is also what Windows ARM64 receives, relying on x64 emulation) — other host platforms are unsupported",
                executable.display(),
                supported_hosts()
            )));
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl crate::Package for Rp2040Picotool {
    async fn ensure_installed(&self) -> fbuild_core::Result<PathBuf> {
        if self.is_installed() {
            return Ok(self.resolved_dir());
        }
        let install_path = self.base.staged_install(Self::validate).await?;
        Ok(find_picotool_root(&install_path))
    }

    fn is_installed(&self) -> bool {
        self.base.is_cached() && self.executable().is_file()
    }

    fn get_info(&self) -> PackageInfo {
        self.base.get_info()
    }
}

struct PlatformPackage {
    filename: &'static str,
    checksum: &'static str,
}

fn all_platform_packages() -> [(&'static str, PlatformPackage); 5] {
    [
        (
            "windows",
            PlatformPackage {
                filename: "x86_64-w64-mingw32.picotool-8a9af99.240929.zip",
                checksum: "d4a43c8172f6b32de412a08e4deac4ef50218f5955c9cda85411b252fcecaea3",
            },
        ),
        (
            "macos-arm64",
            PlatformPackage {
                filename: "aarch64-apple-darwin20.4.picotool-8a9af99.240929.tar.gz",
                checksum: "71eb93270747c5910893f36f5552affd4c254f085b4a7850765b29eec28040ec",
            },
        ),
        (
            "macos-x86_64",
            PlatformPackage {
                filename: "x86_64-apple-darwin20.4.picotool-8a9af99.240929.tar.gz",
                checksum: "a8d30f63e421901000d2b2520f047d1dc586f827f41a3ef52056fd92272ff051",
            },
        ),
        (
            "linux-aarch64",
            PlatformPackage {
                filename: "aarch64-linux-gnu.picotool-8a9af99.240929.tar.gz",
                checksum: "1f73e2c6ce8c7503678dfacec3d2ea889e0f5a161912eff68b290cf405206094",
            },
        ),
        (
            "linux-x86_64",
            PlatformPackage {
                filename: "x86_64-linux-gnu.picotool-8a9af99.240929.tar.gz",
                checksum: "4c5b43afd1e9dba149753089c9715e110f2612cbd47fa005fb033adbe5237ad8",
            },
        ),
    ]
}

/// Comma-separated host keys with a pinned picotool asset; kept derived from
/// `all_platform_packages` so failure messages never drift from the table.
fn supported_hosts() -> String {
    all_platform_packages()
        .iter()
        .map(|(key, _)| *key)
        .collect::<Vec<_>>()
        .join(", ")
}

fn platform_package() -> PlatformPackage {
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

    all_platform_packages()
        .into_iter()
        .find(|(candidate, _)| *candidate == key)
        .map(|(_, package)| package)
        .expect("no managed picotool package for current platform")
}

fn find_picotool_root(install_dir: &Path) -> PathBuf {
    if install_dir.join(picotool_name()).is_file() {
        return install_dir.to_path_buf();
    }
    let nested = install_dir.join("picotool");
    if nested.join(picotool_name()).is_file() {
        return nested;
    }
    install_dir.to_path_buf()
}

fn picotool_name() -> &'static str {
    if cfg!(windows) {
        "picotool.exe"
    } else {
        "picotool"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Package;

    #[test]
    fn package_is_pinned_for_every_supported_host() {
        for (_, package) in all_platform_packages() {
            assert_eq!(package.checksum.len(), 64);
            assert!(package.checksum.chars().all(|c| c.is_ascii_hexdigit()));
            assert!(package.filename.contains("picotool-8a9af99"));
        }
    }

    #[test]
    fn finds_nested_archive_root() {
        let temp = tempfile::tempdir().unwrap();
        let nested = temp.path().join("picotool");
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(nested.join(picotool_name()), []).unwrap();
        assert_eq!(find_picotool_root(temp.path()), nested);
    }

    #[test]
    fn uncached_package_is_not_installed() {
        let temp = tempfile::tempdir().unwrap();
        let package = Rp2040Picotool::with_cache_root(temp.path(), &temp.path().join("cache"));
        assert!(!package.is_installed());
    }

    #[test]
    fn validate_failure_names_the_supported_hosts() {
        let temp = tempfile::tempdir().unwrap();
        let error = Rp2040Picotool::validate(temp.path()).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("windows"));
        assert!(message.contains("macos-arm64"));
        assert!(message.contains("macos-x86_64"));
        assert!(message.contains("linux-aarch64"));
        assert!(message.contains("linux-x86_64"));
        assert!(message.contains("Windows ARM64"));
    }
}
