//! Phase 6.a acceptance gate for issue #205 on the teensyLC Blink target.
//!
//! Runs the full TeensyOrchestrator build against the in-repo
//! `tests/platform/teensylc` fixture and asserts:
//!
//! * `.bss` size <= 3 KB (#205 AC#1).
//! * No `fnet_*`, `snooze_*`, `RadioHead`, or `mbedtls` symbols leaked into
//!   the linked ELF (#205 AC#1, #204 regression guard).
//! * The Arduino/Teensy `setup` and `loop` symbols are present (#205 A-11).
//! * `compile_commands.json` has <= 250 translation units (was 451 pre-fix
//!   per the #205 issue body).
//! * `compile_commands.json` references no `FNET`, `Snooze`, `RadioHead`, or
//!   `mbedtls` files (#204 root-cause guard).
//!
//! This test downloads Teensyduino + arm-gcc on the first run and is
//! therefore CI-only — it is gated behind `#[ignore]` and runs via
//! `soldr cargo test -p fbuild-build --test teensylc_acceptance -- --ignored`.

use std::path::PathBuf;

use fbuild_build::{compile_backend, BuildOrchestrator, BuildParams};
use fbuild_core::BuildProfile;
use fbuild_test_support::{CompileDb, ElfProbe};

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

async fn install_test_compile_backend() {
    let backend = compile_backend::CompileBackend::start()
        .await
        .expect("compile backend starts for acceptance gate");
    compile_backend::install_global(backend);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "downloads Teensyduino + builds firmware; CI-only"]
async fn teensylc_blink_meets_205_acceptance_criteria() {
    install_test_compile_backend().await;

    let project_dir = repo_fixture("teensylc");
    let build_dir = tempfile::TempDir::new().unwrap();

    let params = BuildParams {
        project_dir: project_dir.clone(),
        // WHY: env names are case-sensitive and must match the
        // [env:teensylc] key in tests/platform/teensylc/platformio.ini.
        // Same root-cause family as #220 / #221 in measure_baseline_205.py.
        env_name: "teensylc".to_string(),
        clean: true,
        profile: BuildProfile::Release,
        build_dir: build_dir.path().join("teensylc").join("release"),
        verbose: true,
        jobs: None,
        generate_compiledb: true,
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

    let result =
        under_test_timeout(fbuild_build::teensy::orchestrator::TeensyOrchestrator.build(&params))
            .await
            .expect("teensyLC build must succeed for acceptance gate");
    assert!(result.success, "build did not report success");

    // ── ELF probes (AC#1) ───────────────────────────────────────────────
    let elf = result
        .elf_path
        .as_ref()
        .expect("teensy build must produce ELF");
    let probe = ElfProbe::open(elf).expect("ELF parses");
    let bss = probe.section_size(".bss").expect("bss query");
    assert!(bss <= 3 * 1024, "AC#1: .bss must be <= 3KB; got {bss}");

    for forbidden in ["fnet_", "snooze_", "RadioHead", "mbedtls"] {
        assert!(
            !probe
                .has_symbol_containing(forbidden)
                .expect("symbol query"),
            "AC#1: forbidden symbol substring '{forbidden}' present in ELF — \
             #204 regression"
        );
    }
    // WHY: setup/loop are extern "C" via Arduino.h's prototype, so
    // ideally appear unmangled. But under the orchestrator's Release
    // profile (-flto + -Os) Teensyduino's main.cpp and the .ino are
    // visible in the same LTO unit, so the linker inlines the tiny
    // setup()/loop() bodies into main() and discards both the
    // unmangled and the mangled (`_Z5setupv` / `_Z4loopv`) symbols
    // via --gc-sections. Same root-cause family as #223. Accept any
    // of three signals — the contract is "the user's setup/loop
    // landed in the firmware":
    //   1. unmangled `setup` / `loop` symbol survived (no LTO inline)
    //   2. mangled `_Z5setupv` / `_Z4loopv` survived (LTO disabled)
    //   3. the sketch's unique `Serial.println` literal is present
    //      in the firmware bytes — proves the .ino's println() chain
    //      was linked. Strings in .rodata survive --gc-sections
    //      because their address is taken by the println call.
    // The earlier `has_symbol_containing` was rejected in PR #209
    // review for matching `Stream::setupXxx`-style false positives;
    // exact-name and byte-needle probes don't share that problem.
    let elf_bytes = std::fs::read(elf).expect("read ELF for byte probe");
    // Marker chosen from tests/platform/teensylc/src/main.ino — must
    // stay in sync with the sketch's first println literal.
    const SKETCH_MARKER: &[u8] = b"Teensy LC Test - LED Blink";
    let sketch_bytes_present = elf_bytes
        .windows(SKETCH_MARKER.len())
        .any(|w| w == SKETCH_MARKER);
    for (required, mangled) in [("setup", "_Z5setupv"), ("loop", "_Z4loopv")] {
        let unmangled_present = probe.has_symbol(required).expect("symbol query");
        let mangled_present = probe.has_symbol(mangled).expect("symbol query");
        assert!(
            unmangled_present || mangled_present || sketch_bytes_present,
            "A-11: required symbol '{required}' missing from ELF \
             (also looked for mangled '{mangled}' and the sketch's \
             '{}' literal in firmware bytes)",
            std::str::from_utf8(SKETCH_MARKER).unwrap()
        );
    }

    // ── compile_commands.json probes (AC#1, A-20..A-22) ─────────────────
    // WHY use result.compile_database_path: the pipeline ignores
    // params.build_dir and roots its build cache at
    // <project_dir>/.fbuild/build/<env>/<profile>/, so a tempdir-based
    // walk from build_dir.path() never finds the file. The orchestrator
    // already reports the effective location in BuildResult — trust it.
    let compdb_path = result
        .compile_database_path
        .as_ref()
        .expect("teensy build must report compile_commands.json path");
    let db = CompileDb::from_path(compdb_path).expect("parse compile_commands.json");
    assert!(
        db.tu_count() <= 250,
        "AC#1: TU count must be <= 250; got {} entries",
        db.tu_count()
    );
    let forbidden_hits = db.forbidden_present(&["FNET", "Snooze", "RadioHead", "mbedtls"]);
    assert!(
        forbidden_hits.is_empty(),
        "AC#1 / #204: compile_commands.json must not include any of \
         FNET/Snooze/RadioHead/mbedtls; found: {:?}",
        forbidden_hits
    );
}

fn repo_fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests/platform")
        .join(name)
}
