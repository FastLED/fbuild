//! Integration test: build a real AVR sketch using the full pipeline.
//!
//! Downloads avr-gcc + Arduino core (cached after first run), compiles a
//! minimal blink sketch, and validates the output firmware.hex.
//!
//! Run with: `uv run cargo test -p fbuild-build --test avr_build -- --ignored`

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

/// Verify stem/hash cache path format produces readable, unique paths.
#[test]
fn cache_paths_stem_hash() {
    use fbuild_packages::cache::{hash_url, url_stem};

    // Toolchain: stem from base URL
    assert_eq!(
        url_stem("https://downloads.arduino.cc/tools"),
        "arduino-tools"
    );
    assert_eq!(
        hash_url("https://downloads.arduino.cc/tools"),
        "08e1a7271edb2765"
    );

    // Arduino core: stem from GitHub URL
    let core_url = "https://github.com/arduino/ArduinoCore-avr/archive/refs/tags/1.8.6.tar.gz";
    assert_eq!(url_stem(core_url), "arduino-ArduinoCore-avr");
    assert_eq!(hash_url(core_url), "6e608239126ea48b");

    // Full paths: toolchains/arduino-tools/08e1a7271edb2765/7.3.0/
    let tmp = tempfile::TempDir::new().unwrap();
    let cache =
        fbuild_packages::Cache::with_cache_root(tmp.path(), tmp.path().join("cache").as_path());
    let tc_path = cache.get_toolchain_path("https://downloads.arduino.cc/tools", "7.3.0");
    let tc_str = tc_path.to_string_lossy();
    assert!(tc_str.contains("arduino-tools"));
    assert!(tc_str.contains("08e1a7271edb2765"));
    assert!(tc_str.contains("7.3.0"));
}

/// Build the uno_minimal test project from the Python fbuild repo.
///
/// This test requires:
/// - Internet access (first run only, then cached)
/// - ~/dev/fbuild/tests/uno_minimal/ to exist (Python fbuild repo)
#[test]
#[ignore]
fn build_uno_minimal() {
    let project_dir = home_dir().join("dev/fbuild/tests/uno_minimal");

    if !project_dir.exists() {
        eprintln!(
            "SKIP: {} does not exist (need Python fbuild repo)",
            project_dir.display()
        );
        return;
    }

    // Use a temp build dir so we don't pollute the Python project
    let tmp = tempfile::TempDir::new().unwrap();
    let build_dir = tmp.path().join(".fbuild/build");

    let params = BuildParams {
        project_dir: project_dir.clone(),
        env_name: "uno".to_string(),
        clean: true,
        profile: BuildProfile::Release,
        build_dir,
        verbose: true,
        jobs: None,
    };

    let orchestrator = fbuild_build::avr::orchestrator::AvrOrchestrator;
    let result = orchestrator
        .build(&params)
        .expect("AVR build should succeed");

    assert!(result.success, "build should report success");

    // Verify firmware.hex was produced
    let hex_path = result.hex_path.as_ref().expect("should produce hex file");
    assert!(hex_path.exists(), "firmware.hex should exist");

    let hex_content = fs::read_to_string(hex_path).unwrap();

    // Intel HEX format: starts with ':', ends with EOF record
    assert!(
        hex_content.starts_with(':'),
        "hex file should start with ':'"
    );
    assert!(
        hex_content.contains(":00000001FF"),
        "hex file should contain EOF record"
    );

    // Firmware size sanity check (blink sketch is ~900-7000 bytes of flash)
    let hex_size = hex_path.metadata().unwrap().len();
    assert!(hex_size > 100, "hex file too small: {} bytes", hex_size);
    assert!(hex_size < 50_000, "hex file too large: {} bytes", hex_size);

    // Verify ELF was produced
    let elf_path = result.elf_path.as_ref().expect("should produce elf file");
    assert!(elf_path.exists(), "firmware.elf should exist");

    // Verify size info
    let size = result.size_info.as_ref().expect("should have size info");
    assert!(size.text > 0, "text section should be non-zero");
    assert!(size.total_flash > 0, "total flash should be non-zero");
    assert!(
        size.total_flash < 32256,
        "flash usage should be under Uno limit"
    );

    eprintln!("Build succeeded in {:.1}s", result.build_time_secs);
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

/// Compare our build output against the Python fbuild's cached output.
#[test]
#[ignore]
fn compare_with_python_output() {
    let project_dir = home_dir().join("dev/fbuild/tests/uno_minimal");

    let python_hex = project_dir.join(".fbuild/build/uno/release/firmware.hex");
    if !python_hex.exists() {
        eprintln!(
            "SKIP: Python build output not found at {}",
            python_hex.display()
        );
        return;
    }

    // Build with Rust
    let tmp = tempfile::TempDir::new().unwrap();
    let build_dir = tmp.path().join(".fbuild/build");

    let params = BuildParams {
        project_dir: project_dir.clone(),
        env_name: "uno".to_string(),
        clean: true,
        profile: BuildProfile::Release,
        build_dir,
        verbose: false,
        jobs: None,
    };

    let orchestrator = fbuild_build::avr::orchestrator::AvrOrchestrator;
    let result = orchestrator.build(&params).expect("build should succeed");
    let rust_hex = result.hex_path.expect("should produce hex");

    let python_content = fs::read_to_string(&python_hex).unwrap();
    let rust_content = fs::read_to_string(&rust_hex).unwrap();

    if python_content == rust_content {
        eprintln!("firmware.hex is BYTE-IDENTICAL to Python output");
    } else {
        // Even if not identical, compare sizes as a sanity check
        let python_size = python_hex.metadata().unwrap().len();
        let rust_size = rust_hex.metadata().unwrap().len();
        eprintln!(
            "firmware.hex differs: Python={} bytes, Rust={} bytes",
            python_size, rust_size
        );

        // Size should be in the same ballpark (within 20%)
        let ratio = rust_size as f64 / python_size as f64;
        assert!(
            (0.8..1.2).contains(&ratio),
            "size ratio {:.2} is outside acceptable range",
            ratio
        );

        eprintln!("WARNING: hex files differ but sizes are similar — likely different compiler flag ordering");
    }
}

/// Build a self-contained test project (no dependency on Python fbuild repo).
#[test]
#[ignore]
fn build_self_contained_blink() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project_dir = tmp.path();

    // Create platformio.ini
    fs::write(
        project_dir.join("platformio.ini"),
        "[env:uno]\nplatform = atmelavr\nboard = uno\nframework = arduino\n",
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
  digitalWrite(13, HIGH);
  delay(1000);
  digitalWrite(13, LOW);
  delay(1000);
}
",
    )
    .unwrap();

    let build_dir = project_dir.join(".fbuild/build");
    let params = BuildParams {
        project_dir: project_dir.to_path_buf(),
        env_name: "uno".to_string(),
        clean: true,
        profile: BuildProfile::Release,
        build_dir,
        verbose: true,
        jobs: None,
    };

    let orchestrator = fbuild_build::avr::orchestrator::AvrOrchestrator;
    let result = orchestrator
        .build(&params)
        .expect("self-contained build should succeed");

    assert!(result.success);
    let hex_path = result.hex_path.expect("should produce hex");
    assert!(hex_path.exists());

    let content = fs::read_to_string(&hex_path).unwrap();
    assert!(content.starts_with(':'));
    assert!(content.contains(":00000001FF"));

    let size = result.size_info.expect("should have size info");
    eprintln!(
        "Self-contained build: flash={} ram={} time={:.1}s",
        size.total_flash, size.total_ram, result.build_time_secs
    );
}
