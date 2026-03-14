//! Integration test: build a real Teensy sketch using the full pipeline.
//!
//! Downloads arm-gcc + Teensy cores (cached after first run), compiles a
//! minimal blink sketch, and validates the output firmware.hex.
//!
//! Run with: `uv run cargo test -p fbuild-build --test teensy_build -- --ignored`

use std::fs;
use std::path::PathBuf;

use fbuild_build::{BuildOrchestrator, BuildParams};
use fbuild_core::BuildProfile;

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

    let build_dir = project_dir.join(".fbuild/build");
    let params = BuildParams {
        project_dir: project_dir.to_path_buf(),
        env_name: "teensy41".to_string(),
        clean: true,
        profile: BuildProfile::Release,
        build_dir,
        verbose: true,
        jobs: None,
    };

    let orchestrator = fbuild_build::teensy::orchestrator::TeensyOrchestrator;
    let result = orchestrator
        .build(&params)
        .expect("Teensy build should succeed");

    assert!(result.success);
    let hex_path = result.hex_path.expect("should produce hex");
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
        .join("tests/teensy41");

    if !project_dir.exists() {
        eprintln!("SKIP: {} does not exist", project_dir.display());
        return;
    }

    let tmp = tempfile::TempDir::new().unwrap();
    let build_dir = tmp.path().join(".fbuild/build");

    let params = BuildParams {
        project_dir: project_dir.clone(),
        env_name: "teensy41".to_string(),
        clean: true,
        profile: BuildProfile::Release,
        build_dir,
        verbose: true,
        jobs: None,
    };

    let orchestrator = fbuild_build::teensy::orchestrator::TeensyOrchestrator;
    let result = orchestrator
        .build(&params)
        .expect("Teensy fixture build should succeed");

    assert!(result.success);
    assert!(result.hex_path.is_some());

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
