//! Integration test for FastLED/fbuild#243: stripping eh_frame on ESP32
//! should drop firmware.bin by at least 150 KB and shrink the
//! `.eh_frame` ELF section by at least 50 KB on a stock Blink build.
//!
//! `#[ignore]`-marked because it requires the ESP32 toolchain (~hundreds
//! of MB on first run) and is not headless-CI-friendly. Run with:
//!
//! ```
//! soldr cargo test -p fbuild-build --test eh_frame_strip_esp32 -- --ignored
//! ```
//!
//! The orchestrator invocation pattern mirrors
//! `crates/fbuild-build/tests/esp32_build.rs::build_esp32dev_blink`.

use std::fs;
use std::path::Path;

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

fn make_params(project_dir: &Path) -> BuildParams {
    let build_dir = project_dir.join(".fbuild/build/esp32dev/release");
    BuildParams {
        project_dir: project_dir.to_path_buf(),
        env_name: "esp32dev".to_string(),
        clean_all: false,
        clean_only: false,
        clean: true,
        profile: BuildProfile::Release,
        build_dir,
        verbose: false,
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
    }
}

fn write_blink_project(project_dir: &std::path::Path) {
    fs::write(
        project_dir.join("platformio.ini"),
        "[env:esp32dev]\nplatform = espressif32\nboard = esp32dev\nframework = arduino\n",
    )
    .unwrap();

    let src_dir = project_dir.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    fs::write(
        src_dir.join("blink.cpp"),
        "\
#include <Arduino.h>

void setup() {
  pinMode(2, OUTPUT);
}

void loop() {
  digitalWrite(2, HIGH);
  delay(1000);
  digitalWrite(2, LOW);
  delay(1000);
}
",
    )
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "downloads ESP32 toolchain (~hundreds of MB)"]
async fn eh_frame_strip_drops_firmware_at_least_150kb() {
    // Use two separate tempdirs so .fbuild/build/... paths don't collide.
    let preserve_tmp = tempfile::TempDir::new().unwrap();
    let strip_tmp = tempfile::TempDir::new().unwrap();
    let preserve_dir = preserve_tmp.path().to_path_buf();
    let strip_dir = strip_tmp.path().to_path_buf();

    write_blink_project(&preserve_dir);
    write_blink_project(&strip_dir);

    let orchestrator = fbuild_build::esp32::orchestrator::Esp32Orchestrator;

    // --- Build 1: preserve eh_frame ---
    // SAFETY: tests in the same process may share env vars; we restore them
    // after each build. The test runner doesn't parallelise this binary's
    // test cases at the env-var level (cargo serializes the env touches in
    // this single-test crate), but this is still best-effort hygiene.
    std::env::set_var("FBUILD_KEEP_EH_FRAME", "1");
    std::env::remove_var("FBUILD_STRIP_EH_FRAME");
    let preserve_params = make_params(&preserve_dir);
    let preserve_result = under_test_timeout(orchestrator.build(&preserve_params))
        .await
        .expect("preserve build should succeed");
    assert!(
        preserve_result.success,
        "preserve build should report success"
    );
    let preserve_elf = preserve_result
        .elf_path
        .clone()
        .expect("preserve build should produce ELF path");
    let preserve_firmware_bin = preserve_dir.join(".fbuild/build/esp32dev/release/firmware.bin");
    std::env::remove_var("FBUILD_KEEP_EH_FRAME");

    // --- Build 2: strip eh_frame ---
    std::env::set_var("FBUILD_STRIP_EH_FRAME", "1");
    std::env::remove_var("FBUILD_KEEP_EH_FRAME");
    let strip_params = make_params(&strip_dir);
    let strip_result = under_test_timeout(orchestrator.build(&strip_params))
        .await
        .expect("strip build should succeed");
    assert!(strip_result.success, "strip build should report success");
    let strip_elf = strip_result
        .elf_path
        .clone()
        .expect("strip build should produce ELF path");
    let strip_firmware_bin = strip_dir.join(".fbuild/build/esp32dev/release/firmware.bin");
    std::env::remove_var("FBUILD_STRIP_EH_FRAME");

    // --- firmware.bin delta ---
    let preserve_bin = fs::metadata(&preserve_firmware_bin)
        .expect("preserve firmware.bin should exist")
        .len();
    let strip_bin = fs::metadata(&strip_firmware_bin)
        .expect("strip firmware.bin should exist")
        .len();
    let delta = preserve_bin.saturating_sub(strip_bin);
    assert!(
        delta >= 150 * 1024,
        "expected >=150 KB firmware.bin savings, got {} B (preserve={}, strip={})",
        delta,
        preserve_bin,
        strip_bin,
    );

    // --- ELF .eh_frame section delta ---
    use object::{Object, ObjectSection};
    let preserve_elf_bytes = fs::read(&preserve_elf).expect("read preserve ELF");
    let strip_elf_bytes = fs::read(&strip_elf).expect("read strip ELF");
    let preserve_obj = object::File::parse(&*preserve_elf_bytes).expect("parse preserve ELF");
    let strip_obj = object::File::parse(&*strip_elf_bytes).expect("parse strip ELF");
    let preserve_eh = preserve_obj
        .section_by_name(".eh_frame")
        .map_or(0, |s| s.size());
    let strip_eh = strip_obj
        .section_by_name(".eh_frame")
        .map_or(0, |s| s.size());
    let eh_delta = preserve_eh.saturating_sub(strip_eh);
    assert!(
        eh_delta >= 50 * 1024,
        ".eh_frame section delta must be >=50 KB (got {} B; preserve={} strip={})",
        eh_delta,
        preserve_eh,
        strip_eh,
    );
}
