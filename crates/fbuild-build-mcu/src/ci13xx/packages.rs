//! CI13xx pinned package data and supported host selection.
use fbuild_core::{FbuildError, Result};
use fbuild_packages::{CacheSubdir, MarkerArchivePackage, PackageBase};
use std::path::Path;

pub(super) fn framework(project: &Path) -> MarkerArchivePackage {
    package(
        project,
        "arduino-ci13xx",
        "1.0.18",
        "https://github.com/coloz/arduino-ci130x/releases/download/v1.0.18/arduino-ci130x-1.0.18.zip",
        "cdb2c05644aa34ae2cd4c5eafe5ea0684669d8a88ab9b2004e50b7dd6c43fc56",
        CacheSubdir::Platforms,
        "platform.txt",
    )
}

pub(super) fn toolchain(project: &Path) -> Result<MarkerArchivePackage> {
    let host = fbuild_core::platform::host::current();
    if host.os() != fbuild_core::platform::host::HostOs::Linux
        || host.arch() != fbuild_core::platform::host::HostArch::X86_64
    {
        return Err(FbuildError::PackageError(
            "CI13xx native toolchain currently supports Linux x86_64 only".into(),
        ));
    }
    Ok(package(
        project,
        "nuclei-gcc",
        "9.2.0",
        "https://github.com/coloz/arduino-ci130x/releases/download/v1.0.10/riscv-nuclei-elf-gcc-9.2.0-linux-x86_64.tar.gz",
        "2e24642906cd0c11ff1cc85ebd1b058d18ea75893f9c62f7077c7a65374ca268",
        CacheSubdir::Toolchains,
        "bin/riscv-nuclei-elf-gcc",
    ))
}

fn package(
    project: &Path,
    name: &str,
    version: &str,
    url: &str,
    checksum: &str,
    subdir: CacheSubdir,
    marker: &'static str,
) -> MarkerArchivePackage {
    MarkerArchivePackage::new(
        PackageBase::new(name, version, url, url, Some(checksum), subdir, project),
        marker,
    )
}
