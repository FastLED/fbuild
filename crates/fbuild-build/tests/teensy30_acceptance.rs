//! Acceptance gate for #205 AC#2: teensy30 AnalogOutput sketch.
//!
//! Runs the full TeensyOrchestrator build against an inline-tempdir
//! `AnalogOutput.ino` sketch for the Teensy 3.0 (`teensy30` env) and asserts:
//!
//! * `.dmabuffers` section size <= 1 KB (#205 AC#2). The Teensy 3.0 has
//!   only 16 KB of SRAM; FNET, Snooze, and friends each pull DMAMEM-tagged
//!   statics (DMA descriptor pools, Ethernet frame buffers, RNG state)
//!   into the `.dmabuffers` section. If those libraries are linked into a
//!   simple Arduino `analogWrite` sketch, `.dmabuffers` balloons and the
//!   build blows the RAM budget. This is the AC#2 gate.
//! * No `fnet_*`, `snooze_*`, `RadioHead`, or `mbedtls` symbols leaked
//!   into the linked ELF (#204 / #205 AC#1 regression guard — the same
//!   forbidden list as `teensylc_acceptance.rs`, complementing teensyLC's
//!   `.bss <= 3 KB` gate with the teensy30 `.dmabuffers` gate).
//! * `compile_commands.json` parses and references no `FNET`, `Snooze`,
//!   `RadioHead`, or `mbedtls` files (#204 root-cause guard).
//!
//! Uses the stm32-style inline tempdir `project_dir` so the committed
//! `tests/platform/teensy30/` fixture is untouched and no
//! `compile_commands.json` or `.fbuild/` is ever left behind in the repo.
//!
//! Run with:
//! `soldr cargo test -p fbuild-build --test teensy30_acceptance -- --ignored`
//!
//! Marked `#[ignore]` because it downloads Teensyduino + arm-gcc on the
//! first run (cached after) and performs a full firmware build — too
//! heavy for default `cargo test`.
//!
//! LTO-symbol caveat: as with `teensylc_acceptance.rs` and
//! `stm32_acceptance.rs` (see #223), the Release profile's
//! `-flto -Os` inlines tiny functions like the sketch's `setup` and
//! `loop` into their callers and `--gc-sections` strips the
//! independent symbols. The meaningful signals are therefore the
//! ELF section size and forbidden-symbol substring checks, not
//! probes for `setup`/`loop`/`analogWrite` symbols.

use fbuild_build::{BuildOrchestrator, BuildParams, compile_backend};
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
#[ignore = "downloads Teensyduino + arm-gcc; CI-only"]
async fn teensy30_analog_output_meets_205_ac2() {
    install_test_compile_backend().await;

    // Use a temporary project dir so the committed teensy30 fixture
    // at tests/platform/teensy30/ stays untouched and no scratch
    // build artifacts land in the repo.
    let tmp = tempfile::TempDir::new().unwrap();
    let project_dir = tmp.path();

    std::fs::write(
        project_dir.join("platformio.ini"),
        "[env:teensy30]\n\
         platform = teensy\n\
         board = teensy30\n\
         framework = arduino\n",
    )
    .unwrap();

    let src = project_dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    // WHY .ino: the AC#2 sketch is "AnalogOutput" and Teensyduino's
    // builder treats .ino as Arduino main; this matches the user-facing
    // `fbuild build teensy30 AnalogOutput` invocation in the #205 body.
    std::fs::write(
        src.join("main.ino"),
        "#include <Arduino.h>\n\
         void setup() { pinMode(LED_BUILTIN, OUTPUT); }\n\
         void loop() {\n\
           for (int v = 0; v < 256; v += 5) {\n\
             analogWrite(LED_BUILTIN, v);\n\
             delay(20);\n\
           }\n\
         }\n",
    )
    .unwrap();

    let build_dir = project_dir.join(".fbuild/build/teensy30/release");
    let params = BuildParams {
        project_dir: project_dir.to_path_buf(),
        // WHY env_name = "teensy30": must match the [env:teensy30] key
        // in the platformio.ini we just wrote. Same root-cause family
        // as #220 / #221.
        env_name: "teensy30".to_string(),
        clean_all: false,
        clean_only: false,
        clean: true,
        profile: BuildProfile::Release,
        build_dir,
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
            .expect("teensy30 AnalogOutput build must succeed for AC#2 gate");
    assert!(result.success, "build did not report success");

    // ── ELF probes (AC#2 + #204 regression guard) ───────────────────────
    let elf = result
        .elf_path
        .as_ref()
        .expect("teensy build must produce ELF");
    let probe = ElfProbe::open(elf).expect("ELF parses");

    let dmabuffers = probe
        .section_size(".dmabuffers")
        .expect("dmabuffers section query");
    assert!(
        dmabuffers <= 1024,
        "AC#2: .dmabuffers must be <= 1 KB; got {dmabuffers} bytes. \
         If this fires, the resolver linked FNET/Snooze/RadioHead/mbedtls \
         DMAMEM-tagged statics into a simple analogWrite sketch — see #204."
    );

    for forbidden in ["fnet_", "snooze_", "RadioHead", "mbedtls"] {
        assert!(
            !probe
                .has_symbol_containing(forbidden)
                .expect("symbol query"),
            "AC#2 / #204: forbidden symbol substring '{forbidden}' present \
             in ELF — resolver regression"
        );
    }

    // ── compile_commands.json probes (#204 root-cause guard) ────────────
    // WHY use result.compile_database_path: per #226, the pipeline ignores
    // params.build_dir for the compdb location and roots its build cache
    // at <project_dir>/.fbuild/build/<env>/<profile>/. The orchestrator
    // already reports the effective location in BuildResult — trust it
    // instead of walking the tempdir.
    let compdb_path = result
        .compile_database_path
        .as_ref()
        .expect("teensy build must report compile_commands.json path");
    let db = CompileDb::from_path(compdb_path).expect("parse compile_commands.json");
    let forbidden_hits = db.forbidden_present(&["FNET", "Snooze", "RadioHead", "mbedtls"]);
    assert!(
        forbidden_hits.is_empty(),
        "AC#2 / #204: compile_commands.json must not include any of \
         FNET/Snooze/RadioHead/mbedtls; found: {:?}",
        forbidden_hits
    );
}
