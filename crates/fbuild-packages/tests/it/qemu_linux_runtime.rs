//! Linux end-to-end check: fbuild can start Espressif QEMU on a host that
//! does **not** carry libslirp / libSDL2 / libpixman.
//!
//! This is the regression test for FastLED/FastLED#3964's root cause. The
//! Espressif QEMU tarballs bundle no shared libraries and carry no `RPATH`,
//! and `ubuntu-24.04` — `ubuntu-latest` on GitHub Actions — ships none of the
//! three libraries above. Before fbuild provisioned them itself, every QEMU
//! emulation lane died at exec with
//! `error while loading shared libraries: libslirp.so.0`, and the only fix
//! was an `apt-get install` step *outside* fbuild.
//!
//! The test is meaningful only on a host missing those libraries, which is
//! exactly what the `qemu-linux-runtime.yml` workflow provides: a stock
//! `ubuntu-latest` runner with no apt preinstall step. On a developer machine
//! that already has the libraries the test still passes — it just proves the
//! host path rather than the bundle path, which is also a behaviour worth
//! keeping (fbuild must not download the bundle it does not need).
#![cfg(target_os = "linux")]

use std::path::Path;

use fbuild_toolchain::toolchain::{
    EspQemu, EspQemuArch, build_linux_qemu_ld_library_path, qemu_linux_runtime_lib_dir,
};

/// Run `<qemu> --version` the way the emulator runner does, and report
/// whether it started.
fn qemu_starts(qemu: &Path, project_dir: &Path) -> (bool, String) {
    let ld_library_path = build_linux_qemu_ld_library_path(
        project_dir,
        std::env::var("LD_LIBRARY_PATH").ok().as_deref(),
    );
    let env: Option<Vec<(&str, &str)>> = ld_library_path
        .as_deref()
        .map(|value| vec![("LD_LIBRARY_PATH", value)]);

    let out = fbuild_core::subprocess::run_command_blocking(
        &[&qemu.to_string_lossy(), "--version"],
        None,
        env.as_deref(),
        Some(std::time::Duration::from_secs(10)),
    )
    .expect("probe should spawn");
    (out.success(), format!("{}{}", out.stdout, out.stderr))
}

#[tokio::test]
#[ignore = "downloads Espressif QEMU (~15 MB) and, on a host missing the libraries, the runtime bundle (~5 MB)"]
async fn esp_qemu_starts_without_any_host_library_install() {
    let project = tempfile::TempDir::new().expect("temp project dir");

    for arch in [EspQemuArch::Xtensa, EspQemuArch::Riscv32] {
        let qemu = EspQemu::new(project.path(), arch)
            .expect("package handle")
            .resolve_executable()
            .await
            .unwrap_or_else(|e| panic!("{arch:?}: fbuild could not provide a usable QEMU: {e}"));

        let (started, output) = qemu_starts(&qemu, project.path());
        assert!(
            started,
            "{arch:?}: {} could not start during invocation:\n{output}",
            qemu.display()
        );
        assert!(
            output.contains("QEMU emulator version"),
            "{arch:?}: unexpected --version output:\n{output}"
        );
    }

    // On a host that needed the bundle, it must now be installed and exported
    // through LD_LIBRARY_PATH; on a host that did not, fbuild must not have
    // downloaded it at all. Both are correct — what would not be correct is a
    // bundle that exists but never reaches the QEMU invocation.
    if let Some(lib_dir) = qemu_linux_runtime_lib_dir(project.path()) {
        assert!(
            lib_dir.join("libslirp.so.0").is_file(),
            "installed bundle is missing libslirp.so.0: {}",
            lib_dir.display()
        );
        let ld = build_linux_qemu_ld_library_path(project.path(), None)
            .expect("installed bundle must produce an LD_LIBRARY_PATH");
        assert!(
            ld.starts_with(&lib_dir.to_string_lossy().to_string()),
            "bundle must come first on LD_LIBRARY_PATH, got: {ld}"
        );
    }
}
