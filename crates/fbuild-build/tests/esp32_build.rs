//! Integration test: build a real ESP32 sketch using the full pipeline.
//!
//! Downloads esp32 toolchain + framework (cached after first run), compiles a
//! minimal blink sketch, and validates the output firmware.bin.
//!
//! Run with: `soldr cargo test -p fbuild-build --test esp32_build -- --ignored`

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

    let build_dir = project_dir.join(".fbuild/build/esp32dev/release");
    let params = BuildParams {
        project_dir: project_dir.to_path_buf(),
        env_name: "esp32dev".to_string(),
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

    let orchestrator = fbuild_build::esp32::orchestrator::Esp32Orchestrator;
    let result = orchestrator
        .build(&params)
        .expect("ESP32 build should succeed");

    assert!(result.success);
    let elf_path = result.firmware_path.expect("should produce ELF file");
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

    let build_dir = project_dir.join(".fbuild/build/esp32c6/release");
    let params = BuildParams {
        project_dir: project_dir.to_path_buf(),
        env_name: "esp32c6".to_string(),
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

    let orchestrator = fbuild_build::esp32::orchestrator::Esp32Orchestrator;
    let result = orchestrator
        .build(&params)
        .expect("ESP32-C6 build should succeed");

    assert!(result.success);
    let bin_path = result.firmware_path.expect("should produce bin file");
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

/// Build a self-contained ESP32-C3 blink sketch (RISC-V).
///
/// ESP32-C3 uses the rv32imc RISC-V ISA.  This test validates the full build
/// pipeline for the C3 variant, including toolchain selection and framework
/// extraction.  It requires Internet access (first run only, then cached).
#[test]
#[ignore]
fn build_esp32c3_blink() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project_dir = tmp.path();

    fs::write(
        project_dir.join("platformio.ini"),
        "[env:esp32c3]\nplatform = espressif32\nboard = esp32-c3-devkitm-1\nframework = arduino\n",
    )
    .unwrap();

    let src_dir = project_dir.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    fs::write(
        src_dir.join("blink.cpp"),
        "\
#include <Arduino.h>

void setup() {
  // GPIO 8 is commonly available on ESP32-C3 DevKit
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

    let build_dir = project_dir.join(".fbuild/build/esp32c3/release");
    let params = BuildParams {
        project_dir: project_dir.to_path_buf(),
        env_name: "esp32c3".to_string(),
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

    let orchestrator = fbuild_build::esp32::orchestrator::Esp32Orchestrator;
    let result = orchestrator
        .build(&params)
        .expect("ESP32-C3 build should succeed");

    assert!(result.success);
    let bin_path = result.firmware_path.expect("should produce bin file");
    assert!(bin_path.exists());

    let size = result.size_info.expect("should have size info");
    eprintln!(
        "ESP32-C3 blink build: flash={}/{} ({:.1}%) ram={}/{} ({:.1}%) time={:.1}s",
        size.total_flash,
        size.max_flash.unwrap_or(0),
        size.flash_percent().unwrap_or(0.0),
        size.total_ram,
        size.max_ram.unwrap_or(0),
        size.ram_percent().unwrap_or(0.0),
        result.build_time_secs
    );
}

/// Build a self-contained ESP32-S3 blink sketch (Xtensa, native USB-CDC).
///
/// This test requires Internet access (first run only, then cached).
#[test]
#[ignore]
fn build_esp32s3_blink() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project_dir = tmp.path();

    // Create platformio.ini for ESP32-S3-DevKitC-1
    fs::write(
        project_dir.join("platformio.ini"),
        "[env:esp32s3]\nplatform = espressif32\nboard = esp32-s3-devkitc-1\nframework = arduino\n",
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
  Serial.begin(115200);
  pinMode(2, OUTPUT);
}

void loop() {
  digitalWrite(2, HIGH);
  delay(500);
  digitalWrite(2, LOW);
  delay(500);
  Serial.println(\"Hello from ESP32-S3!\");
}
",
    )
    .unwrap();

    let build_dir = project_dir.join(".fbuild/build/esp32s3/release");
    let params = BuildParams {
        project_dir: project_dir.to_path_buf(),
        env_name: "esp32s3".to_string(),
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

    let orchestrator = fbuild_build::esp32::orchestrator::Esp32Orchestrator;
    let result = orchestrator
        .build(&params)
        .expect("ESP32-S3 build should succeed");

    assert!(result.success);
    let elf_path = result.firmware_path.expect("should produce ELF file");
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
        "ESP32-S3 blink build: flash={}/{} ({:.1}%) ram={}/{} ({:.1}%) time={:.1}s",
        size.total_flash,
        size.max_flash.unwrap_or(0),
        size.flash_percent().unwrap_or(0.0),
        size.total_ram,
        size.max_ram.unwrap_or(0),
        size.ram_percent().unwrap_or(0.0),
        result.build_time_secs
    );
}

/// Build ESP32-S3 blink from the tests/platform/esp32s3 fixture with persistent output.
///
/// Build output is stored at tests/platform/esp32s3/.fbuild/build/ for manual deployment.
#[test]
#[ignore]
fn build_esp32s3_fixture() {
    let project_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests/platform/esp32s3");

    if !project_dir.exists() {
        eprintln!("SKIP: {} does not exist", project_dir.display());
        return;
    }

    let build_dir = project_dir.join(".fbuild/build/esp32s3/release");
    let params = BuildParams {
        project_dir: project_dir.clone(),
        env_name: "esp32s3".to_string(),
        clean: true,
        profile: BuildProfile::Release,
        build_dir: build_dir.clone(),
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

    let orchestrator = fbuild_build::esp32::orchestrator::Esp32Orchestrator;
    let result = orchestrator
        .build(&params)
        .expect("ESP32-S3 fixture build should succeed");

    assert!(result.success);
    let elf_path = result.firmware_path.expect("should produce ELF file");
    assert!(elf_path.exists());

    eprintln!("ESP32-S3 firmware ELF at: {}", elf_path.display());

    let size = result.size_info.expect("should have size info");
    eprintln!(
        "ESP32-S3 fixture build: flash={}/{} ({:.1}%) ram={}/{} ({:.1}%) time={:.1}s",
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
    let build_dir = tmp.path().join(".fbuild/build/demo/release");

    let params = BuildParams {
        project_dir: project_dir.clone(),
        env_name: "demo".to_string(),
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

    let orchestrator = fbuild_build::esp32::orchestrator::Esp32Orchestrator;
    let result = orchestrator
        .build(&params)
        .expect("NightDriverStrip demo build should succeed");

    assert!(result.success, "build should report success");

    let bin_path = result
        .firmware_path
        .as_ref()
        .expect("should produce bin file");
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

/// Incremental build of NightDriverStrip — no source changes.
///
/// Requires a prior clean build to exist at ~/dev/NightDriverStrip/.fbuild/build/demo/.
/// Measures how fast a no-op rebuild is (should be seconds, not minutes).
#[test]
#[ignore]
fn incremental_nightdriverstrip_no_changes() {
    // Try both NightDriverStrip locations
    let project_dir = home_dir().join("dev/NightDriverStrip");
    let env_name = if project_dir.exists() {
        "demo".to_string()
    } else {
        let alt = home_dir().join("dev/fbuild/tests/NightDriverStrip");
        if !alt.exists() {
            eprintln!("SKIP: no NightDriverStrip project found");
            return;
        }
        // Use the test copy
        return incremental_build_at(&alt, "demo");
    };

    incremental_build_at(&project_dir, &env_name);
}

fn incremental_build_at(project_dir: &std::path::Path, env_name: &str) {
    // Verify there's an existing build
    let build_marker = project_dir
        .join(".fbuild/build")
        .join(env_name)
        .join("release/firmware.elf");
    if !build_marker.exists() {
        eprintln!(
            "SKIP: no prior build at {} (run clean build first)",
            build_marker.display()
        );
        return;
    }

    let params = BuildParams {
        project_dir: project_dir.to_path_buf(),
        env_name: env_name.to_string(),
        clean: false,
        profile: BuildProfile::Release,
        build_dir: fbuild_paths::BuildLayout::new(
            project_dir.to_path_buf(),
            env_name.to_string(),
            BuildProfile::Release,
        )
        .resolve(),
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

    let orchestrator = fbuild_build::esp32::orchestrator::Esp32Orchestrator;
    let result = orchestrator
        .build(&params)
        .expect("incremental build should succeed");

    assert!(result.success, "incremental build should succeed");

    eprintln!(
        "\n=== INCREMENTAL BUILD (no changes) ===\nTime: {:.2}s\n",
        result.build_time_secs
    );

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

    // Incremental with no changes should be fast (under 30 seconds)
    assert!(
        result.build_time_secs < 30.0,
        "incremental build too slow: {:.1}s (expected < 30s)",
        result.build_time_secs
    );
}

/// Incremental build with a single source file touched.
///
/// Touches one .cpp file to simulate a single-file edit, then rebuilds.
/// This measures the real incremental compile + relink time.
#[test]
#[ignore]
fn incremental_nightdriverstrip_one_file_changed() {
    let project_dir = home_dir().join("dev/NightDriverStrip");
    if !project_dir.exists() {
        eprintln!("SKIP: ~/dev/NightDriverStrip does not exist");
        return;
    }

    let env_name = "demo";
    let build_marker = project_dir
        .join(".fbuild/build")
        .join(env_name)
        .join("release/firmware.elf");
    if !build_marker.exists() {
        eprintln!("SKIP: no prior build at {}", build_marker.display());
        return;
    }

    // Touch one source file to trigger recompilation
    let src_dir = project_dir.join("src");
    if let Ok(entries) = std::fs::read_dir(&src_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "cpp" || e == "h") {
                // Touch the file by writing it back unchanged
                if let Ok(content) = std::fs::read(&path) {
                    std::fs::write(&path, &content).ok();
                    eprintln!("touched: {}", path.display());
                    break;
                }
            }
        }
    }

    let params = BuildParams {
        project_dir: project_dir.clone(),
        env_name: env_name.to_string(),
        clean: false,
        profile: BuildProfile::Release,
        build_dir: fbuild_paths::BuildLayout::new(
            project_dir.clone(),
            env_name.to_string(),
            BuildProfile::Release,
        )
        .resolve(),
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

    let orchestrator = fbuild_build::esp32::orchestrator::Esp32Orchestrator;
    let result = orchestrator
        .build(&params)
        .expect("incremental build should succeed");

    assert!(result.success, "incremental build should succeed");

    eprintln!(
        "\n=== INCREMENTAL BUILD (one file touched) ===\nTime: {:.2}s\n",
        result.build_time_secs
    );

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
}
