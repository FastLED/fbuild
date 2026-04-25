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
//! `uv run soldr cargo test -p fbuild-build --test teensylc_acceptance -- --ignored`.

use std::path::PathBuf;

use fbuild_build::{BuildOrchestrator, BuildParams};
use fbuild_core::BuildProfile;
use fbuild_test_support::{CompileDb, ElfProbe};

#[test]
#[ignore = "downloads Teensyduino + builds firmware; CI-only"]
fn teensylc_blink_meets_205_acceptance_criteria() {
    let project_dir = repo_fixture("teensylc");
    let build_dir = tempfile::TempDir::new().unwrap();

    let params = BuildParams {
        project_dir: project_dir.clone(),
        env_name: "teensyLC".to_string(),
        clean: true,
        profile: BuildProfile::Release,
        build_dir: build_dir.path().to_path_buf(),
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
    };

    let result = fbuild_build::teensy::orchestrator::TeensyOrchestrator
        .build(&params)
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
    for required in ["setup", "loop"] {
        assert!(
            probe.has_symbol(required).expect("symbol query")
                || probe.has_symbol_containing(required).expect("symbol query"),
            "A-11: required symbol '{required}' missing from ELF"
        );
    }

    // ── compile_commands.json probes (AC#1, A-20..A-22) ─────────────────
    let compdb_path = locate_compile_commands(build_dir.path(), "teensyLC")
        .expect("compile_commands.json should land in build dir");
    let db = CompileDb::from_path(&compdb_path).expect("parse compile_commands.json");
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

fn locate_compile_commands(build_dir: &std::path::Path, env: &str) -> Option<PathBuf> {
    // Per fbuild's layout the file lives at one of:
    //   <build_dir>/<env>/compile_commands.json
    //   <build_dir>/compile_commands.json
    // Search both, prefer the per-env path.
    let candidates = [
        build_dir.join(env).join("compile_commands.json"),
        build_dir.join("compile_commands.json"),
    ];
    for c in candidates {
        if c.exists() {
            return Some(c);
        }
    }
    // Fallback: walk the build_dir for any compile_commands.json.
    for entry in walkdir::WalkDir::new(build_dir).into_iter().flatten() {
        if entry.file_name() == "compile_commands.json" {
            return Some(entry.into_path());
        }
    }
    None
}
