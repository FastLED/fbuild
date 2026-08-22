//! Linux runtime shared libraries for Espressif QEMU.
//!
//! The Espressif QEMU tarballs ship the emulator binary, its ROM blobs and a
//! static `libfdt.a` — and nothing else. The binaries carry no `RPATH`/
//! `RUNPATH` and dynamically link five non-glibc libraries:
//!
//! ```text
//! libpixman-1.so.0  libgcrypt.so.20  libSDL2-2.0.so.0  libz.so.1  libslirp.so.0
//! ```
//!
//! On a stock `ubuntu-24.04` image (what `ubuntu-latest` resolves to on GitHub
//! Actions) libslirp, libSDL2 and libpixman are all absent, so
//! `qemu-system-xtensa` dies at exec with
//! `error while loading shared libraries: libslirp.so.0`.
//!
//! Making callers `apt-get install` the set first turns QEMU emulation into
//! something you have to bootstrap from outside fbuild. Instead fbuild
//! downloads a prebuilt bundle of the full non-glibc dependency closure and
//! points the emulator at it with `LD_LIBRARY_PATH`.
//!
//! The bundle is **lazy**: it is only downloaded when a probe proves the host
//! cannot start QEMU on its own. Hosts that already carry the libraries never
//! fetch it and never have their libraries shadowed.
//!
//! Bundle provenance: `ci/build_qemu_linux_runtime.py` walks `ldd` over both
//! real QEMU binaries inside `ubuntu:22.04` and archives the transitive
//! closure minus the glibc family. glibc and the loader deliberately stay on
//! the host — a bundled `libc.so.6` without its matching `ld-linux` is a
//! segfault, not a fix.

use std::path::{Path, PathBuf};

use fbuild_core::platform::host::{self, HostArch, HostPlatform};
use fbuild_core::{FbuildError, Result};

use crate::{CacheSubdir, Package, PackageBase, PackageInfo};

/// Release tag of the QEMU build this bundle was closed over. Kept in sync
/// with `QEMU_RELEASE_TAG` in `esp_qemu.rs` and with
/// `ci/build_qemu_linux_runtime.py`.
const RUNTIME_VERSION: &str = "esp-develop-9.2.2-20250817";

/// Release that hosts the prebuilt bundles.
const RUNTIME_RELEASE_TAG: &str = "qemu-linux-runtime-v1";

/// Subdirectory the archive extracts its libraries into.
const LIB_SUBDIR: &str = "lib";

/// One library that must be present for the bundle to be considered valid —
/// it is the one whose absence broke CI in the first place.
const SENTINEL_LIB: &str = "libslirp.so.0";

/// The bundled Linux runtime libraries for Espressif QEMU.
pub struct QemuLinuxRuntime {
    base: PackageBase,
}

impl QemuLinuxRuntime {
    pub fn new(project_dir: &Path) -> Result<Self> {
        Self::for_host(host::current(), project_dir)
    }

    fn for_host(host: HostPlatform, project_dir: &Path) -> Result<Self> {
        let arch = runtime_arch(host)?;
        let url = format!(
            "https://github.com/FastLED/fbuild/releases/download/{}/qemu-esp-linux-runtime-{}-{}.tar.zst",
            RUNTIME_RELEASE_TAG, arch, RUNTIME_VERSION
        );
        Ok(Self {
            base: PackageBase::new(
                "esp-qemu-linux-runtime",
                RUNTIME_VERSION,
                &url,
                &format!("qemu-linux-runtime-{arch}"),
                Some(runtime_sha256(arch)?),
                CacheSubdir::Toolchains,
                project_dir,
            ),
        })
    }

    /// Directory to place on `LD_LIBRARY_PATH`.
    pub fn lib_dir(&self) -> PathBuf {
        self.base.install_path().join(LIB_SUBDIR)
    }

    /// Install if needed and return the directory to put on `LD_LIBRARY_PATH`.
    pub async fn ensure_lib_dir(&self) -> Result<PathBuf> {
        self.ensure_installed().await?;
        Ok(self.lib_dir())
    }
}

#[async_trait::async_trait]
impl Package for QemuLinuxRuntime {
    async fn ensure_installed(&self) -> Result<PathBuf> {
        if self.is_installed() {
            return Ok(self.base.install_path());
        }
        self.base.staged_install(validate_runtime_install).await
    }

    fn is_installed(&self) -> bool {
        self.base.is_cached() && self.lib_dir().join(SENTINEL_LIB).is_file()
    }

    fn get_info(&self) -> PackageInfo {
        self.base.get_info()
    }
}

/// Reject an extracted tree that does not carry the libraries it exists for.
fn validate_runtime_install(install_dir: &Path) -> Result<()> {
    let lib_dir = install_dir.join(LIB_SUBDIR);
    if lib_dir.join(SENTINEL_LIB).is_file() {
        return Ok(());
    }
    Err(FbuildError::PackageError(format!(
        "QEMU Linux runtime bundle at {} is incomplete: {}/{} not found",
        install_dir.display(),
        LIB_SUBDIR,
        SENTINEL_LIB,
    )))
}

/// Architecture token used in the bundle's asset name.
fn runtime_arch(host: HostPlatform) -> Result<&'static str> {
    if !host.is_linux() {
        return Err(FbuildError::PackageError(format!(
            "the QEMU runtime-library bundle is Linux-only; host is {}",
            host.os_name()
        )));
    }
    match host.arch() {
        HostArch::X86_64 => Ok("x86_64"),
        HostArch::Aarch64 => Ok("aarch64"),
        _ => Err(FbuildError::PackageError(format!(
            "no QEMU runtime-library bundle is published for linux-{}",
            host.arch_name()
        ))),
    }
}

/// SHA-256 of each published bundle. An architecture without an entry has no
/// bundle yet: report that plainly instead of downloading something unpinned.
///
/// aarch64 is built by `.github/workflows/qemu-runtime-bundle.yml` on an
/// `ubuntu-22.04-arm` runner — Docker Desktop's arm64 emulation cannot run
/// dpkg's maintainer scripts, so it cannot be produced from a developer
/// workstation the way the x86_64 bundle was.
fn runtime_sha256(arch: &str) -> Result<&'static str> {
    match arch {
        "x86_64" => Ok("e4f22c9b88a1a032dcba07aec2ac7ada01563b7fe6ccdd3a1a0b3d740aec51df"),
        other => Err(FbuildError::PackageError(format!(
            "no QEMU runtime-library bundle is published for linux-{other} yet (tracked in the {RUNTIME_RELEASE_TAG} release).\n\
             Install the QEMU runtime libraries from your distribution — on Debian/Ubuntu: libslirp0, libsdl2-2.0-0, libpixman-1-0."
        ))),
    }
}

/// Prepend `lib_dir` to an existing `LD_LIBRARY_PATH` value.
///
/// Returns the combined value. An empty or absent current value yields just
/// the bundle directory. The bundle goes first because the host is, by
/// construction, missing at least one of the libraries it carries.
pub fn ld_library_path_with(lib_dir: &Path, current: Option<&str>) -> String {
    let bundle = lib_dir.to_string_lossy().to_string();
    match current {
        Some(existing) if !existing.is_empty() => format!("{bundle}:{existing}"),
        _ => bundle,
    }
}

/// Spawn-time lookup: the bundle directory, if this host needed it and it is
/// already installed.
///
/// Stateless on purpose — mirrors `build_windows_qemu_path_env`, so the
/// emulator spawn path does not have to thread a resolution result through
/// every call site. A host that never needed the bundle has nothing cached
/// here and gets `None`.
pub fn installed_lib_dir(project_dir: &Path) -> Option<PathBuf> {
    let runtime = QemuLinuxRuntime::new(project_dir).ok()?;
    if runtime.is_installed() {
        Some(runtime.lib_dir())
    } else {
        None
    }
}

/// `LD_LIBRARY_PATH` for a QEMU spawn, or `None` when the host does not need
/// the bundle.
pub fn build_linux_qemu_ld_library_path(
    project_dir: &Path,
    current: Option<&str>,
) -> Option<String> {
    let lib_dir = installed_lib_dir(project_dir)?;
    Some(ld_library_path_with(&lib_dir, current))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linux(arch: HostArch) -> HostPlatform {
        HostPlatform::new(fbuild_core::platform::host::HostOs::Linux, arch)
    }

    #[test]
    fn arch_token_for_linux_hosts() {
        assert_eq!(runtime_arch(linux(HostArch::X86_64)).unwrap(), "x86_64");
        assert_eq!(runtime_arch(linux(HostArch::Aarch64)).unwrap(), "aarch64");
    }

    #[test]
    fn non_linux_hosts_are_rejected() {
        let win = HostPlatform::new(
            fbuild_core::platform::host::HostOs::Windows,
            HostArch::X86_64,
        );
        let err = runtime_arch(win).unwrap_err().to_string();
        assert!(err.contains("Linux-only"), "unexpected error: {err}");
    }

    #[test]
    fn url_points_at_the_published_asset() {
        let tmp = tempfile::TempDir::new().unwrap();
        let rt = QemuLinuxRuntime::for_host(linux(HostArch::X86_64), tmp.path()).unwrap();
        let info = rt.get_info();
        assert!(
            info.url.ends_with(&format!(
                "qemu-esp-linux-runtime-x86_64-{RUNTIME_VERSION}.tar.zst"
            )),
            "unexpected url: {}",
            info.url
        );
        assert!(info.url.contains(RUNTIME_RELEASE_TAG), "url: {}", info.url);
    }

    #[test]
    fn ld_library_path_puts_bundle_first() {
        let combined = ld_library_path_with(Path::new("/cache/rt/lib"), Some("/usr/local/lib"));
        assert_eq!(combined, "/cache/rt/lib:/usr/local/lib");
    }

    #[test]
    fn ld_library_path_without_existing_value() {
        assert_eq!(
            ld_library_path_with(Path::new("/cache/rt/lib"), None),
            "/cache/rt/lib"
        );
        assert_eq!(
            ld_library_path_with(Path::new("/cache/rt/lib"), Some("")),
            "/cache/rt/lib"
        );
    }

    #[test]
    fn validate_rejects_tree_without_sentinel_library() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(LIB_SUBDIR)).unwrap();
        let err = validate_runtime_install(tmp.path())
            .unwrap_err()
            .to_string();
        assert!(err.contains(SENTINEL_LIB), "unexpected error: {err}");
    }

    #[test]
    fn validate_accepts_tree_with_sentinel_library() {
        let tmp = tempfile::TempDir::new().unwrap();
        let lib = tmp.path().join(LIB_SUBDIR);
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(lib.join(SENTINEL_LIB), b"").unwrap();
        validate_runtime_install(tmp.path()).expect("complete tree should validate");
    }

    #[test]
    fn uninstalled_bundle_yields_no_ld_library_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Nothing is cached under a fresh project dir, so the spawn-time
        // lookup must stay quiet rather than inventing a path.
        assert!(build_linux_qemu_ld_library_path(tmp.path(), Some("/usr/lib")).is_none());
    }
}
