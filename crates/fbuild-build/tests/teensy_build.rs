//! Integration test: build a real Teensy sketch using the full pipeline.
//!
//! Downloads arm-gcc + Teensy cores (cached after first run), compiles a
//! minimal blink sketch, and validates the output firmware.hex.
//!
//! Run with: `soldr cargo test -p fbuild-build --test teensy_build -- --ignored`

use std::fs;
use std::path::{Path, PathBuf};

use fbuild_build::{BuildOrchestrator, BuildParams};
use fbuild_core::BuildProfile;

fn copy_dir_recursive(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path);
        } else {
            fs::copy(&src_path, &dst_path).unwrap();
        }
    }
}

/// Build a self-contained Teensy 4.1 blink sketch.
///
/// This test requires Internet access (first run only, then cached).
#[test]
#[ignore]
fn build_teensy41_blink() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project_dir = tmp.path();

    // Create platformio.ini
    fs::write(
        project_dir.join("platformio.ini"),
        "[env:teensy41]\nplatform = teensy\nboard = teensy41\nframework = arduino\n",
    )
    .unwrap();

    // Create src/blink.ino
    let src_dir = project_dir.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    fs::write(
        src_dir.join("blink.ino"),
        "\
void setup() {
  pinMode(13, OUTPUT);
}

void loop() {
  digitalWriteFast(13, HIGH);
  delay(500);
  digitalWriteFast(13, LOW);
  delay(500);
}
",
    )
    .unwrap();

    let build_dir = project_dir.join(".fbuild/build/teensy41/release");
    let params = BuildParams {
        project_dir: project_dir.to_path_buf(),
        env_name: "teensy41".to_string(),
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
    };

    let orchestrator = fbuild_build::teensy::orchestrator::TeensyOrchestrator;
    let result = orchestrator
        .build(&params)
        .expect("Teensy build should succeed");

    assert!(result.success);
    let hex_path = result.firmware_path.expect("should produce hex");
    assert!(hex_path.exists());

    let content = fs::read_to_string(&hex_path).unwrap();
    assert!(content.starts_with(':'));
    assert!(content.contains(":00000001FF"));

    let size = result.size_info.expect("should have size info");
    eprintln!(
        "Teensy 4.1 build: flash={} ram={} time={:.1}s",
        size.total_flash, size.total_ram, result.build_time_secs
    );
}

/// Build a Teensy 4.1 sketch that includes Teensyduino framework libraries.
#[test]
#[ignore]
fn build_teensy41_spi_octo_headers() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project_dir = tmp.path();

    fs::write(
        project_dir.join("platformio.ini"),
        "[env:teensy41]\nplatform = teensy\nboard = teensy41\nframework = arduino\n",
    )
    .unwrap();

    let src_dir = project_dir.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    fs::write(
        src_dir.join("main.cpp"),
        "\
#include <Arduino.h>
#include <SPI.h>
#include <OctoWS2811.h>

void setup() {
  SPI.begin();
}

void loop() {}
",
    )
    .unwrap();

    let build_dir = project_dir.join(".fbuild/build/teensy41/release");
    let params = BuildParams {
        project_dir: project_dir.to_path_buf(),
        env_name: "teensy41".to_string(),
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
    };

    let orchestrator = fbuild_build::teensy::orchestrator::TeensyOrchestrator;
    let result = orchestrator
        .build(&params)
        .expect("Teensy framework library headers should build");

    assert!(result.success);
    assert!(result.firmware_path.expect("should produce hex").exists());
}

/// Build using Teensy test fixture from the repo.
#[test]
#[ignore]
fn build_teensy41_fixture() {
    // Use the repo's test fixture
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let project_dir = manifest_dir
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests/platform/teensy41");

    if !project_dir.exists() {
        eprintln!("SKIP: {} does not exist", project_dir.display());
        return;
    }

    let tmp = tempfile::TempDir::new().unwrap();
    let build_dir = tmp.path().join(".fbuild/build/teensy41/release");

    let params = BuildParams {
        project_dir: project_dir.clone(),
        env_name: "teensy41".to_string(),
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
    };

    let orchestrator = fbuild_build::teensy::orchestrator::TeensyOrchestrator;
    let result = orchestrator
        .build(&params)
        .expect("Teensy fixture build should succeed");

    assert!(result.success);
    assert!(result.firmware_path.is_some());

    if let Some(ref size) = result.size_info {
        eprintln!(
            "Size: flash={}/{} ({:.1}%) ram={}/{} ({:.1}%)",
            size.total_flash,
            size.max_flash.unwrap_or(0),
            size.flash_percent().unwrap_or(0.0),
            size.total_ram,
            size.max_ram.unwrap_or(0),
            size.ram_percent().unwrap_or(0.0),
        );
    }

    eprintln!("Build succeeded in {:.1}s", result.build_time_secs);
}

/// Build a Teensy 3.0 fixture where a project-local lib/FastLED shadows the bundled framework.
#[test]
#[ignore]
fn build_teensy30_fixture_prefers_local_fastled() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fixture_dir = manifest_dir
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests/platform/teensy30");

    if !fixture_dir.exists() {
        eprintln!("SKIP: {} does not exist", fixture_dir.display());
        return;
    }

    let tmp = tempfile::TempDir::new().unwrap();
    let project_dir = tmp.path().join("project");
    copy_dir_recursive(&fixture_dir, &project_dir);

    fs::create_dir_all(project_dir.join("lib/FastLED/src")).unwrap();
    fs::write(
        project_dir.join("lib/FastLED/src/FastLED.h"),
        "\
#pragma once
#include <Arduino.h>

namespace fastled_fixture {
void begin();
}
",
    )
    .unwrap();
    fs::write(
        project_dir.join("lib/FastLED/src/FastLED.cpp"),
        "\
#include <FastLED.h>

namespace fastled_fixture {
void begin() {
  pinMode(LED_BUILTIN, OUTPUT);
}
}
",
    )
    .unwrap();
    fs::write(
        project_dir.join("src/main.ino"),
        "\
#include <FastLED.h>

void setup() {
  fastled_fixture::begin();
}

void loop() {
  digitalWrite(LED_BUILTIN, HIGH);
  delay(500);
  digitalWrite(LED_BUILTIN, LOW);
  delay(500);
}
",
    )
    .unwrap();

    let params = BuildParams {
        project_dir: project_dir.clone(),
        env_name: "teensy30".to_string(),
        clean: true,
        profile: BuildProfile::Release,
        build_dir: tmp.path().join(".fbuild/build/teensy30/release"),
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
    };

    let orchestrator = fbuild_build::teensy::orchestrator::TeensyOrchestrator;
    let result = orchestrator
        .build(&params)
        .expect("Teensy 3.0 local FastLED shadow build should succeed");

    assert!(result.success);
    let firmware_path = result.firmware_path.expect("should produce hex");
    assert!(firmware_path.exists());
    let build_dir = result
        .elf_path
        .as_ref()
        .and_then(|path| path.parent())
        .expect("elf path should live in the build output directory")
        .to_path_buf();

    let local_fastled_objects: Vec<_> = fs::read_dir(build_dir.join("lib").join("FastLED"))
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.starts_with("FastLED_") && name.ends_with(".cpp.o"))
        .collect();
    assert!(
        !local_fastled_objects.is_empty(),
        "expected local lib/FastLED to compile"
    );

    let framework_fastled_objects: Vec<_> = fs::read_dir(build_dir.join("core"))
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.starts_with("FastLED_") && name.ends_with(".cpp.o"))
        .collect();
    assert!(
        framework_fastled_objects.is_empty(),
        "bundled Teensy framework FastLED should be shadowed by lib/FastLED, found {:?}",
        framework_fastled_objects
    );
}
