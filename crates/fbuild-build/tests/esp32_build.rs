//! Integration test: build a real ESP32 sketch using the full pipeline.
//!
//! Downloads esp32 toolchain + framework (cached after first run), compiles a
//! minimal blink sketch, and validates the output firmware.bin.
//!
//! Run with: `uv run cargo test -p fbuild-build --test esp32_build -- --ignored`

use std::fs;
use std::path::PathBuf;

use fbuild_build::{BuildOrchestrator, BuildParams};
use fbuild_core::BuildProfile;

fn home_dir() -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(std::env::var("USERPROFILE").expect("USERPROFILE not set"))
    }
    #[cfg(not(windows))]
    {
        PathBuf::from(std::env::var("HOME").expect("HOME not set"))
    }
}

/// Build a self-contained ESP32 blink sketch.
///
/// This test requires Internet access (first run only, then cached).
#[test]
#[ignore]
fn build_esp32dev_blink() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project_dir = tmp.path();

    // Create platformio.ini
    fs::write(
        project_dir.join("platformio.ini"),
        "[env:esp32dev]\nplatform = espressif32\nboard = esp32dev\nframework = arduino\n",
    )
    .unwrap();

    // Create src/blink.cpp
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

    let build_dir = project_dir.join(".fbuild/build");
    let params = BuildParams {
        project_dir: project_dir.to_path_buf(),
        env_name: "esp32dev".to_string(),
        clean: true,
        profile: BuildProfile::Release,
        build_dir,
        verbose: true,
        jobs: None,
    };

    let orchestrator = fbuild_build::esp32::orchestrator::Esp32Orchestrator;
    let result = orchestrator
        .build(&params)
        .expect("ESP32 build should succeed");

    assert!(result.success);
    let elf_path = result.hex_path.expect("should produce ELF file");
    assert!(elf_path.exists());

    let elf_size = elf_path.metadata().unwrap().len();
    assert!(
        elf_size > 1000,
        "firmware.elf too small: {} bytes",
        elf_size
    );
    assert!(
        elf_size < 10_000_000,
        "firmware.elf too large: {} bytes",
        elf_size
    );

    let size = result.size_info.expect("should have size info");
    eprintln!(
        "ESP32 blink build: flash={}/{} ({:.1}%) ram={}/{} ({:.1}%) time={:.1}s",
        size.total_flash,
        size.max_flash.unwrap_or(0),
        size.flash_percent().unwrap_or(0.0),
        size.total_ram,
        size.max_ram.unwrap_or(0),
        size.ram_percent().unwrap_or(0.0),
        result.build_time_secs
    );
}

/// Build a self-contained ESP32-C6 blink sketch (RISC-V).
#[test]
#[ignore]
fn build_esp32c6_blink() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project_dir = tmp.path();

    fs::write(
        project_dir.join("platformio.ini"),
        "[env:esp32c6]\nplatform = espressif32\nboard = esp32-c6\nframework = arduino\n",
    )
    .unwrap();

    let src_dir = project_dir.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    fs::write(
        src_dir.join("blink.cpp"),
        "\
#include <Arduino.h>

void setup() {
  pinMode(8, OUTPUT);
}

void loop() {
  digitalWrite(8, HIGH);
  delay(1000);
  digitalWrite(8, LOW);
  delay(1000);
}
",
    )
    .unwrap();

    let build_dir = project_dir.join(".fbuild/build");
    let params = BuildParams {
        project_dir: project_dir.to_path_buf(),
        env_name: "esp32c6".to_string(),
        clean: true,
        profile: BuildProfile::Release,
        build_dir,
        verbose: true,
        jobs: None,
    };

    let orchestrator = fbuild_build::esp32::orchestrator::Esp32Orchestrator;
    let result = orchestrator
        .build(&params)
        .expect("ESP32-C6 build should succeed");

    assert!(result.success);
    let bin_path = result.hex_path.expect("should produce bin file");
    assert!(bin_path.exists());

    let size = result.size_info.expect("should have size info");
    eprintln!(
        "ESP32-C6 blink build: flash={}/{} ({:.1}%) ram={}/{} ({:.1}%) time={:.1}s",
        size.total_flash,
        size.max_flash.unwrap_or(0),
        size.flash_percent().unwrap_or(0.0),
        size.total_ram,
        size.max_ram.unwrap_or(0),
        size.ram_percent().unwrap_or(0.0),
        result.build_time_secs
    );
}

/// Build NightDriverStrip demo environment (ESP32dev, Xtensa).
///
/// Requires ~/dev/fbuild/tests/NightDriverStrip/ to exist.
/// NOTE: This will fail until library dependency resolution (Phase 4) is implemented,
/// because NightDriverStrip depends on FastLED, ArduinoJson, etc.
#[test]
#[ignore]
fn build_nightdriverstrip_demo() {
    let project_dir = home_dir().join("dev/fbuild/tests/NightDriverStrip");

    if !project_dir.exists() {
        eprintln!(
            "SKIP: {} does not exist (need Python fbuild repo)",
            project_dir.display()
        );
        return;
    }

    let tmp = tempfile::TempDir::new().unwrap();
    let build_dir = tmp.path().join(".fbuild/build");

    let params = BuildParams {
        project_dir: project_dir.clone(),
        env_name: "demo".to_string(),
        clean: true,
        profile: BuildProfile::Release,
        build_dir,
        verbose: true,
        jobs: None,
    };

    let orchestrator = fbuild_build::esp32::orchestrator::Esp32Orchestrator;
    let result = orchestrator
        .build(&params)
        .expect("NightDriverStrip demo build should succeed");

    assert!(result.success, "build should report success");

    let bin_path = result.hex_path.as_ref().expect("should produce bin file");
    assert!(bin_path.exists(), "firmware.bin should exist");

    let bin_size = bin_path.metadata().unwrap().len();
    assert!(
        bin_size > 100_000,
        "firmware too small ({} bytes), likely incomplete",
        bin_size
    );

    let size = result.size_info.as_ref().expect("should have size info");
    eprintln!(
        "NightDriverStrip demo: flash={}/{} ({:.1}%) ram={}/{} ({:.1}%) time={:.1}s",
        size.total_flash,
        size.max_flash.unwrap_or(0),
        size.flash_percent().unwrap_or(0.0),
        size.total_ram,
        size.max_ram.unwrap_or(0),
        size.ram_percent().unwrap_or(0.0),
        result.build_time_secs
    );
}
