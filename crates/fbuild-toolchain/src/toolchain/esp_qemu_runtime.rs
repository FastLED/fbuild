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
//! real QEMU binaries inside `ubuntu:20.04` and archives the transitive
//! closure minus the glibc family. glibc and the loader deliberately stay on
//! the host — a bundled `libc.so.6` without its matching `ld-linux` is a
//! segfault, not a fix.
//!
//! The 20.04 build image is chosen so the bundle is never the binding
//! portability constraint: its libraries need at most `GLIBC_2.30`, which is
//! exactly what the Espressif QEMU binaries themselves require. Building on
//! 22.04 would raise the floor to `GLIBC_2.34` and lock out hosts that could
//! otherwise run QEMU.

use std::path::Path;

use fbuild_core::path::NormalizedPath;
use fbuild_core::platform::host::{self, HostArch, HostPlatform};
use fbuild_core::{FbuildError, Result};

use crate::{CacheSubdir, PackageBase, PackageInfo};

/// Release tag of the QEMU build this bundle was closed over. Kept in sync
/// with `QEMU_RELEASE_TAG` in `esp_qemu.rs` and with
/// `ci/build_qemu_linux_runtime.py`.
const RUNTIME_VERSION: &str = "esp-develop-9.2.2-20250817";

/// Release that hosts the prebuilt bundles.
const RUNTIME_RELEASE_TAG: &str = "qemu-linux-runtime-v1";

/// Subdirectory the archive extracts its libraries into.
const LIB_SUBDIR: &str = "lib";

/// Every non-glibc library the Espressif QEMU binaries link directly. All of
/// them must be present for a cached bundle to count as complete: checking one
/// sentinel would let a half-extracted tree pass `is_installed()`, and the
/// install would then never be repaired — the second probe would just fail
/// with "cannot start even with the bundle applied".
///
/// The bundle carries their transitive closure too, but those are reachable
/// only through these five, so a tree that has all five and nothing else is
/// already impossible from `staged_install`'s atomic rename.
const REQUIRED_LIBS: &[&str] = &[
    "libslirp.so.0",
    "libSDL2-2.0.so.0",
    "libpixman-1.so.0",
    "libgcrypt.so.20",
    "libz.so.1",
];

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
    pub fn lib_dir(&self) -> NormalizedPath {
        NormalizedPath::from(self.base.install_path()).join(LIB_SUBDIR)
    }

    /// Install if needed and return the directory to put on `LD_LIBRARY_PATH`.
    pub async fn ensure_lib_dir(&self) -> Result<NormalizedPath> {
        if !self.is_installed() {
            self.base.staged_install(validate_runtime_install).await?;
        }
        Ok(self.lib_dir())
    }

    /// Whether a complete bundle is already unpacked in the cache.
    pub fn is_installed(&self) -> bool {
        self.base.is_cached() && missing_libs(self.lib_dir().as_path()).is_empty()
    }

    pub fn get_info(&self) -> PackageInfo {
        self.base.get_info()
    }
}

/// Which of the required libraries are absent from an extracted `lib/` dir.
fn missing_libs(lib_dir: &Path) -> Vec<&'static str> {
    REQUIRED_LIBS
        .iter()
        .copied()
        .filter(|lib| !lib_dir.join(lib).is_file())
        .collect()
}

/// Reject an extracted tree that does not carry the libraries it exists for.
fn validate_runtime_install(install_dir: &Path) -> Result<()> {
    let lib_dir = install_dir.join(LIB_SUBDIR);
    let missing = missing_libs(&lib_dir);
    if missing.is_empty() {
        return Ok(());
    }
    Err(FbuildError::PackageError(format!(
        "QEMU Linux runtime bundle at {} is incomplete: {} missing from {}/",
        install_dir.display(),
        missing.join(", "),
        LIB_SUBDIR,
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
/// Both are built by `.github/workflows/qemu-runtime-bundle.yml`; aarch64 needs
/// its native arm64 runner because Docker Desktop's arm64 emulation cannot run
/// dpkg's maintainer scripts.
fn runtime_sha256(arch: &str) -> Result<&'static str> {
    match arch {
        "x86_64" => Ok("b3318ccf60df8e17a42b5b0f61180440f56337fadb0babe867bff1f3dfecd99f"),
        "aarch64" => Ok("1782782a25f1a375c03bcdc1aafb7444f46d5f2fa40d66793dd530b76c62da0a"),
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
pub fn installed_lib_dir(project_dir: &Path) -> Option<NormalizedPath> {
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

/// Result of probing a QEMU binary with `--version`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
enum QemuProbe {
    /// The binary started; its shared-library dependencies all resolved.
    Started,
    /// The dynamic linker could not satisfy a dependency (exit code 127).
    /// Carries the linker's own line when it could be recovered.
    MissingSharedLibrary(String),
    /// Probe could not be interpreted (spawn failure, or some other
    /// non-127 exit). Treated as non-fatal: the real run reports it with
    /// full context.
    Inconclusive,
}

/// Probe the QEMU binary with `--version` to verify its shared library
/// dependencies resolve at runtime.
///
/// `lib_dir`, when given, is prepended to `LD_LIBRARY_PATH` for the probe so
/// the caller can ask "does it start *with* the bundled runtime libraries?".
///
/// Probing is Linux-only. Windows resolves its DLLs through the `PATH`
/// hydration path above, and macOS builds are self-contained.
fn probe_qemu_binary(qemu_binary: &Path, lib_dir: Option<&Path>) -> QemuProbe {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (qemu_binary, lib_dir);
        QemuProbe::Started
    }

    #[cfg(target_os = "linux")]
    {
        // Short synchronous probe: verify the QEMU binary can start before we
        // hand it to the async emulator runner. Uses run_command_blocking which
        // routes through containment (no console flash on Windows, containment
        // group on all platforms) and is ~100 ms.
        let ld_library_path = lib_dir
            .map(|dir| ld_library_path_with(dir, std::env::var("LD_LIBRARY_PATH").ok().as_deref()));
        let env: Option<Vec<(&str, &str)>> = ld_library_path
            .as_deref()
            .map(|value| vec![("LD_LIBRARY_PATH", value)]);

        let probe_result = fbuild_core::subprocess::run_command_blocking(
            &[&qemu_binary.to_string_lossy(), "--version"],
            None, // cwd
            env.as_deref(),
            Some(std::time::Duration::from_secs(5)),
        );

        match probe_result {
            Ok(out) if out.success() => QemuProbe::Started,
            Ok(out) if out.exit_code == 127 => {
                let detail = out
                    .stderr
                    .lines()
                    .find(|l| l.contains("error while loading shared libraries"))
                    .map(|l| l.trim().to_string())
                    .unwrap_or_else(|| out.stderr.trim().to_string());
                QemuProbe::MissingSharedLibrary(detail)
            }
            Ok(_) | Err(_) => QemuProbe::Inconclusive,
        }
    }
}

/// Make sure the resolved QEMU binary can actually start on this host.
///
/// The Espressif tarballs bundle no shared libraries and carry no `RPATH`, so
/// on Linux the binary needs libslirp/libSDL2/libpixman/libgcrypt/libz from
/// somewhere. A stock `ubuntu-24.04` has none of the first three. Rather than
/// make every caller `apt-get install` them first — an external bootstrap step
/// fbuild exists to remove — fbuild downloads its own runtime bundle and
/// re-probes with it applied.
///
/// The bundle is fetched lazily: a host that can already start QEMU never
/// downloads it and never has its own libraries shadowed.
pub(crate) async fn ensure_qemu_can_start(qemu_binary: &Path, project_dir: &Path) -> Result<()> {
    let missing = match probe_qemu_binary(qemu_binary, None) {
        QemuProbe::Started | QemuProbe::Inconclusive => return Ok(()),
        QemuProbe::MissingSharedLibrary(detail) => detail,
    };

    tracing::info!(
        "QEMU at {} is missing a host shared library ({}); fetching the fbuild runtime bundle",
        qemu_binary.display(),
        missing
    );

    let runtime = QemuLinuxRuntime::new(project_dir)
        .map_err(|e| runtime_unavailable_error(qemu_binary, &missing, &e.to_string()))?;
    let lib_dir = runtime
        .ensure_lib_dir()
        .await
        .map_err(|e| runtime_unavailable_error(qemu_binary, &missing, &e.to_string()))?;

    match probe_qemu_binary(qemu_binary, Some(lib_dir.as_path())) {
        QemuProbe::Started | QemuProbe::Inconclusive => Ok(()),
        QemuProbe::MissingSharedLibrary(still_missing) => Err(FbuildError::PackageError(format!(
            "QEMU at {} cannot start even with the fbuild runtime bundle at {} applied.\n\
             {}\n\
             The bundle carries the full non-glibc dependency closure of the Espressif QEMU binaries, so this points at a host glibc older than the bundle's build image (ubuntu 22.04, glibc 2.35), or at a library the closure does not cover.\n\
             Please report it at https://github.com/FastLED/fbuild/issues.",
            qemu_binary.display(),
            lib_dir.display(),
            still_missing,
        ))),
    }
}

/// Error for "the host cannot start QEMU and fbuild could not provision the
/// libraries either" — keeps the original linker complaint in view.
fn runtime_unavailable_error(qemu_binary: &Path, missing: &str, cause: &str) -> FbuildError {
    FbuildError::PackageError(format!(
        "QEMU at {} cannot start: a required shared library is missing.\n\
         {}\n\
         fbuild could not provision its Linux runtime bundle: {}",
        qemu_binary.display(),
        missing,
        cause,
    ))
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

    /// Lay down an extracted-bundle tree carrying `libs`.
    fn bundle_tree(root: &Path, libs: &[&str]) {
        let lib = root.join(LIB_SUBDIR);
        std::fs::create_dir_all(&lib).unwrap();
        for name in libs {
            std::fs::write(lib.join(name), b"").unwrap();
        }
    }

    #[test]
    fn validate_rejects_empty_tree_and_names_every_missing_library() {
        let tmp = tempfile::TempDir::new().unwrap();
        bundle_tree(tmp.path(), &[]);
        let err = validate_runtime_install(tmp.path())
            .unwrap_err()
            .to_string();
        for lib in REQUIRED_LIBS {
            assert!(err.contains(lib), "error should name {lib}: {err}");
        }
    }

    #[test]
    fn validate_rejects_tree_missing_one_library() {
        // A partially-restored CI cache must be re-installed, not accepted:
        // otherwise `is_installed()` short-circuits the repair and the QEMU
        // probe fails afterwards with nothing left to fix it.
        let tmp = tempfile::TempDir::new().unwrap();
        bundle_tree(tmp.path(), &REQUIRED_LIBS[1..]);
        let err = validate_runtime_install(tmp.path())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(REQUIRED_LIBS[0]),
            "error should name the one missing library: {err}"
        );
    }

    #[test]
    fn validate_accepts_tree_with_every_required_library() {
        let tmp = tempfile::TempDir::new().unwrap();
        bundle_tree(tmp.path(), REQUIRED_LIBS);
        validate_runtime_install(tmp.path()).expect("complete tree should validate");
    }

    #[test]
    fn uninstalled_bundle_yields_no_ld_library_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Nothing is cached under a fresh project dir, so the spawn-time
        // lookup must stay quiet rather than inventing a path.
        assert!(build_linux_qemu_ld_library_path(tmp.path(), Some("/usr/lib")).is_none());
    }

    // ── probe_qemu_binary ───────────────────────────────────────────

    #[test]
    fn probe_reports_started_when_binary_runs_version_successfully() {
        // On Linux, a real QEMU binary would pass. On non-Linux, probing is
        // a no-op that always reports Started. We use a script that exits 0
        // so the assertion holds cross-platform.
        let tmp = tempfile::TempDir::new().unwrap();
        let probe = tmp.path().join("probe_qemu");
        if fbuild_core::platform::host::is_windows() {
            std::fs::write(&probe, b"@echo off\r\nexit /b 0\r\n").unwrap();
        } else {
            std::fs::write(&probe, b"#!/bin/sh\nexit 0\n").unwrap();
            fbuild_core::platform::fs::set_executable(&probe).unwrap();
        };
        assert!(
            matches!(probe_qemu_binary(&probe, None), QemuProbe::Started),
            "probe should report Started when the binary returns 0"
        );
    }

    #[test]
    fn probe_linux_detects_missing_shared_library_exit_127() {
        if fbuild_core::platform::host::current().os() != fbuild_core::platform::host::HostOs::Linux
        {
            return;
        }
        let tmp = tempfile::TempDir::new().unwrap();
        // Script that prints the canonical dynamic-linker error to stderr
        // and exits 127 — same observable as a missing .so.
        let probe = tmp.path().join("fake_qemu_missing_so");
        std::fs::write(
            &probe,
            b"#!/bin/sh\necho 'error while loading shared libraries: libslirp.so.0: cannot open shared object file' >&2\nexit 127\n",
        )
        .unwrap();
        fbuild_core::platform::fs::set_executable(&probe).unwrap();

        match probe_qemu_binary(&probe, None) {
            QemuProbe::MissingSharedLibrary(detail) => assert!(
                detail.contains("libslirp.so.0"),
                "probe should name the missing library: {detail}"
            ),
            _ => panic!("exit 127 must be reported as a missing shared library"),
        }
    }

    #[test]
    fn probe_linux_exports_the_bundle_on_ld_library_path() {
        if fbuild_core::platform::host::current().os() != fbuild_core::platform::host::HostOs::Linux
        {
            return;
        }
        let tmp = tempfile::TempDir::new().unwrap();
        // Script that succeeds only when LD_LIBRARY_PATH leads with the
        // directory we asked the probe to apply — i.e. the bundle actually
        // reaches the QEMU invocation rather than merely being installed.
        let probe = tmp.path().join("fake_qemu_needs_lib_dir");
        let lib_dir = tmp.path().join("bundle-lib");
        std::fs::create_dir_all(&lib_dir).unwrap();
        std::fs::write(
            &probe,
            format!(
                "#!/bin/sh\ncase \"$LD_LIBRARY_PATH\" in\n  {}:*|{}) exit 0;;\nesac\necho 'error while loading shared libraries: libslirp.so.0' >&2\nexit 127\n",
                lib_dir.display(),
                lib_dir.display()
            )
            .as_bytes(),
        )
        .unwrap();
        fbuild_core::platform::fs::set_executable(&probe).unwrap();

        assert!(
            matches!(
                probe_qemu_binary(&probe, None),
                QemuProbe::MissingSharedLibrary(_)
            ),
            "without the bundle the stub must fail like a real missing .so"
        );
        assert!(
            matches!(
                probe_qemu_binary(&probe, Some(&lib_dir)),
                QemuProbe::Started
            ),
            "probe must put the bundle directory on LD_LIBRARY_PATH"
        );
    }
}
