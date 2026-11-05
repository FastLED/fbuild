//! Pinned packages shared by native build and `fbuild install`.
use fbuild_core::{FbuildError, Result};
use fbuild_packages::{CacheSubdir, Package, PackageBase, PackageInfo};
use std::path::{Path, PathBuf};

pub(super) struct VendorPackage {
    base: PackageBase,
    marker: &'static str,
}

impl VendorPackage {
    pub(super) fn installed_root(&self) -> Result<PathBuf> {
        if !self.is_installed() {
            return Err(FbuildError::PackageError(
                "CI13xx framework not installed; run fbuild install".into(),
            ));
        }
        self.root(&self.base.install_path())
    }

    pub(super) fn framework(project: &Path) -> Self {
        Self::new(
            project,
            "arduino-ci13xx",
            "1.0.18",
            "https://github.com/coloz/arduino-ci130x/releases/download/v1.0.18/arduino-ci130x-1.0.18.zip",
            "cdb2c05644aa34ae2cd4c5eafe5ea0684669d8a88ab9b2004e50b7dd6c43fc56",
            CacheSubdir::Platforms,
            "platform.txt",
        )
    }

    pub(super) fn toolchain(project: &Path) -> Result<Self> {
        let host = fbuild_core::platform::host::current();
        if host.os() != fbuild_core::platform::host::HostOs::Linux
            || host.arch() != fbuild_core::platform::host::HostArch::X86_64
        {
            return Err(FbuildError::PackageError(
                "CI13xx native toolchain currently supports Linux x86_64 only".into(),
            ));
        }
        Ok(Self::new(
            project,
            "nuclei-gcc",
            "9.2.0",
            "https://github.com/coloz/arduino-ci130x/releases/download/v1.0.10/riscv-nuclei-elf-gcc-9.2.0-linux-x86_64.tar.gz",
            "2e24642906cd0c11ff1cc85ebd1b058d18ea75893f9c62f7077c7a65374ca268",
            CacheSubdir::Toolchains,
            "bin/riscv-nuclei-elf-gcc",
        ))
    }

    fn new(
        project: &Path,
        name: &str,
        version: &str,
        url: &str,
        checksum: &str,
        subdir: CacheSubdir,
        marker: &'static str,
    ) -> Self {
        Self {
            base: PackageBase::new(name, version, url, url, Some(checksum), subdir, project),
            marker,
        }
    }

    fn root(&self, installed: &Path) -> Result<PathBuf> {
        for entry in walkdir::WalkDir::new(installed).max_depth(3) {
            let entry = entry.map_err(|e| FbuildError::PackageError(e.to_string()))?;
            if entry.file_type().is_dir() && entry.path().join(self.marker).is_file() {
                return Ok(entry.path().to_path_buf());
            }
        }
        Err(FbuildError::PackageError(format!(
            "CI13xx package missing {} in {}",
            self.marker,
            installed.display()
        )))
    }
}

#[async_trait::async_trait]
impl Package for VendorPackage {
    async fn ensure_installed(&self) -> Result<PathBuf> {
        if self.is_installed() {
            return self.root(&self.base.install_path());
        }
        let installed = self
            .base
            .staged_install(|path| self.root(path).map(|_| ()))
            .await?;
        self.root(&installed)
    }
    fn is_installed(&self) -> bool {
        self.base.is_cached() && self.root(&self.base.install_path()).is_ok()
    }
    fn get_info(&self) -> PackageInfo {
        self.base.get_info()
    }
}
