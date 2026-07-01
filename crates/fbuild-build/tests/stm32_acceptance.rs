//! Acceptance gate for #205 AC#4 / closes #202: stm32f103c8 SPI auto-discovery.
//!
//! This integration test verifies that an stm32f103c8 Blink sketch which
//! `#include`s `<SPI.h>` builds with no manual library allowlist, and that
//! the bundled `Arduino_Core_STM32` SPI library is automatically discovered
//! by fbuild's library-selection layer.
//!
//! Run with:
//! `soldr cargo test -p fbuild-build --test stm32_acceptance -- --ignored`
//!
//! Marked `#[ignore]` because it downloads the ARM GCC toolchain plus the
//! STM32duino cores (cached after first run) and performs a full firmware
//! build — too heavy for default `cargo test`.
//!
//! Acceptance criteria (#205 AC#4):
//! 1. The build succeeds.
//! 2. `compile_commands.json` references at least one source file under
//!    the SPI library (substring `SPI`).
//! 3. The ELF contains evidence that `Arduino_Core_STM32/libraries/SPI/`
//!    was compiled and linked into the firmware (#202, #223). The probe
//!    accepts either a mangled `SPIClass*` C++ symbol (visible without LTO)
//!    or a `PinMap_SPI_*` array from the library's `utility/spi_com.c`
//!    (an LTO-stable global whose address is referenced by the SPI
//!    peripheral pin tables). The Release profile uses
//!    `-flto -Os -fno-rtti`, which inlines `SPIClass::begin()` and friends
//!    into their callers and strips their independent symbols — see #223
//!    for the diagnostic walk-through.

use std::path::{Path, PathBuf};

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
#[ignore = "downloads STM32duino + builds firmware; CI-only"]
async fn stm32f103c8_blink_with_spi_auto_discovers_library_205_ac4() {
    install_test_compile_backend().await;

    // Use a temporary project dir so we can write our own SPI-using sketch
    // independent of whatever ships in the fixture.
    let tmp = tempfile::TempDir::new().unwrap();
    let project_dir = tmp.path();

    std::fs::write(
        project_dir.join("platformio.ini"),
        "[env:stm32f103c8]\n\
         platform = ststm32\n\
         board = bluepill_f103c8\n\
         framework = arduino\n",
    )
    .unwrap();

    let src = project_dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("main.cpp"),
        "#include <Arduino.h>\n\
         #include <SPI.h>\n\
         void setup() { SPI.begin(); }\n\
         void loop() {}\n",
    )
    .unwrap();

    let build_dir = project_dir.join(".fbuild/build/stm32f103c8/release");
    let params = BuildParams {
        project_dir: project_dir.to_path_buf(),
        env_name: "stm32f103c8".to_string(),
        clean: true,
        profile: BuildProfile::Release,
        build_dir: build_dir.clone(),
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

    let orchestrator = fbuild_build::stm32::orchestrator::Stm32Orchestrator;
    let result = under_test_timeout(orchestrator.build(&params))
        .await
        .expect("stm32f103c8 build with SPI must succeed");
    assert!(result.success, "build did not report success");

    let elf = result
        .elf_path
        .as_ref()
        .expect("stm32 build must produce ELF");
    let probe = ElfProbe::open(elf).expect("ELF parses");
    // WHY two-shot: the Release profile's `-flto -Os -fno-rtti` inlines
    // `SPIClass::begin()` (and the other SPI methods called from setup())
    // into their callers and discards the independent mangled symbols. So
    // `SPIClass` substring is reliable in non-LTO builds (Quick) but not
    // in LTO builds (Release). `PinMap_SPI_MOSI` is a `const` global array
    // declared in `Arduino_Core_STM32/libraries/SPI/src/utility/spi_com.c`
    // whose address is taken by the SPI peripheral pin tables — it survives
    // both LTO and `--gc-sections`. Either signal proves the SPI library
    // was discovered, compiled, and linked. See #223 for the trace.
    let has_spiclass = probe
        .has_symbol_containing("SPIClass")
        .expect("symbol query");
    let has_pinmap = probe
        .has_symbol_containing("PinMap_SPI_")
        .expect("symbol query");
    assert!(
        has_spiclass || has_pinmap,
        "AC#4: SPI library must be present in ELF — closes #202; saw \
         neither a mangled `SPIClass*` symbol nor a `PinMap_SPI_*` global \
         (probed both because the Release profile's LTO can inline the \
         former). If only one form is missing, the library is auto-\
         discovered correctly but the probe needs a third candidate."
    );

    let compdb = locate_compile_commands(&build_dir, "stm32f103c8")
        .expect("compile_commands.json should land in build dir");
    let db = CompileDb::from_path(&compdb).expect("parse compile_commands.json");
    let spi_entries: Vec<_> = db.entries_matching("SPI").collect();
    assert!(
        !spi_entries.is_empty(),
        "AC#4: compile_commands.json must reference an SPI library entry — \
         closes #202; found {} entries with no SPI hit",
        db.tu_count()
    );
}

fn locate_compile_commands(build_dir: &Path, env: &str) -> Option<PathBuf> {
    let candidates = [
        build_dir.join(env).join("compile_commands.json"),
        build_dir.join("compile_commands.json"),
    ];
    for c in candidates {
        if c.exists() {
            return Some(c);
        }
    }
    for entry in walkdir::WalkDir::new(build_dir).into_iter().flatten() {
        if entry.file_name() == "compile_commands.json" {
            return Some(entry.into_path());
        }
    }
    None
}
