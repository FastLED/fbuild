//! Regression test for FastLED/fbuild#587: `[env:*] build_flags` from
//! `platformio.ini` must reach the nxplpc orchestrator's **library**
//! compile path (not just the sketch compile path).
//!
//! Before the fix, only the sketch compile in `pipeline::sequential` folded
//! `ctx.user_flags` into its overlay. `crates/fbuild-build/src/nxplpc/
//! orchestrator.rs` constructed `LibraryBuildEnv` with raw
//! `compiler.c_flags()` / `compiler.cpp_flags()`, so library sources reached
//! by `lib_extra_dirs` never saw `-D…` defines declared in `platformio.ini`.
//!
//! That gap is exactly why PR #576 had to stash `-DRELEASE=1
//! -DFASTLED_DISABLE_DBG=1` inside `lpc845brk.json`'s `extra_flags` field —
//! the `extra_flags` board property IS applied to library compile, but user
//! `build_flags` weren't. This test exercises a fixture whose library source
//! `#error`s out unless `-DFROM_PLATFORMIO_INI=1` (set via
//! `[env:lpc845brk] build_flags`) reaches it. A successful build proves the
//! fix is wired end-to-end.
//!
//! Run with:
//! `soldr cargo test -p fbuild-build --test nxplpc_build_flags -- --ignored`
//!
//! Marked `#[ignore]` because it downloads the ARM GCC toolchain plus the
//! vendored ArduinoCore-LPC8xx framework and performs a real firmware
//! build — too heavy for default `cargo test`.

use fbuild_build::{BuildOrchestrator, BuildParams};
use fbuild_core::BuildProfile;

/// 15-min wall-clock cap for `--ignored` real-toolchain tests (FastLED/fbuild#806).
const REAL_BUILD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(900);

async fn under_test_timeout<F: std::future::Future>(fut: F) -> F::Output {
    match tokio::time::timeout(REAL_BUILD_TIMEOUT, fut).await {
        Ok(v) => v,
        Err(_) => panic!(
            "real-toolchain test exceeded {:.0}s budget — see FastLED/fbuild#806",
            REAL_BUILD_TIMEOUT.as_secs_f64()
        ),
    }
}

/// Locate the in-tree fixture so the test is independent of CWD.
fn fixture_dir() -> std::path::PathBuf {
    // CARGO_MANIFEST_DIR is `…/crates/fbuild-build`; the fixture lives at
    // `…/tests/platform/lpc845_build_flags`, two `..` levels above.
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root resolvable from CARGO_MANIFEST_DIR")
        .join("tests/platform/lpc845_build_flags")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "downloads ARM GCC + ArduinoCore-LPC8xx + builds firmware; CI-only"]
async fn lpc845brk_propagates_build_flags_to_library_compile_587() {
    let fixture = fixture_dir();
    assert!(
        fixture.join("platformio.ini").is_file(),
        "fixture missing platformio.ini at {}",
        fixture.display()
    );

    // Build into a temp dir so reruns are clean and don't litter the repo.
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let build_dir = tmp.path().join(".fbuild/build/lpc845brk/release");

    let params = BuildParams {
        project_dir: fixture.clone(),
        env_name: "lpc845brk".to_string(),
        clean_all: false,
        clean_only: false,
        clean: true,
        profile: BuildProfile::Release,
        build_dir,
        verbose: true,
        jobs: None,
        generate_compiledb: false,
        compiledb_only: false,
        log_sender: None,
        symbol_analysis: false,
        symbol_analysis_path: None,
        no_timestamp: false,
        src_dir: None,
        pio_env: Default::default(),
        extra_build_flags: Vec::new(),
        watch_set_cache: None,
        bloat_analysis: false,
    };

    let orchestrator = fbuild_build::nxplpc::orchestrator::NxpLpcOrchestrator;
    let result = under_test_timeout(orchestrator.build(&params))
        .await
        .expect("#587 regression: lpc845brk build with check_flag library must succeed");
    assert!(
        result.success,
        "#587 regression: build did not report success — the `#error` in \
         check_flag.cpp likely fired, meaning `[env:*] build_flags` did NOT \
         reach the nxplpc library compile path"
    );
    assert!(
        result.elf_path.is_some(),
        "#587 regression: build reported success but produced no ELF"
    );
}
