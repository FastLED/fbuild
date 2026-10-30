//! Unit tests for `fbuild uploadfs` — the parts that decide what gets flashed
//! where, before any hardware is involved (FastLED/fbuild#1358).

use super::*;

fn args() -> UploadfsArgs {
    UploadfsArgs {
        project_dir: None,
        environment: None,
        data_dir: None,
        partitions: None,
        fs_partition: None,
        filesystem: None,
        tool: None,
        image: None,
        no_build: false,
        port: None,
        baud: None,
        verify: false,
        json: false,
    }
}

/// `uploadfs` must hand `buildfs` exactly the knobs the user set, so a rebuild
/// can never pack a different tree or a different partition than was planned.
#[test]
fn build_args_mirrors_the_upload_flags() {
    let mut upload = args();
    upload.environment = Some("esp32s3".to_string());
    upload.data_dir = Some("assets".to_string());
    upload.partitions = Some("partitions_custom.csv".to_string());
    upload.fs_partition = Some("storage".to_string());
    upload.filesystem = Some("littlefs".to_string());
    upload.tool = Some("/opt/mklittlefs".to_string());
    upload.image = Some("out/fs.bin".to_string());

    let build = upload.build_args();
    assert_eq!(build.environment.as_deref(), Some("esp32s3"));
    assert_eq!(build.data_dir.as_deref(), Some("assets"));
    assert_eq!(build.partitions.as_deref(), Some("partitions_custom.csv"));
    assert_eq!(build.fs_partition.as_deref(), Some("storage"));
    assert_eq!(build.filesystem.as_deref(), Some("littlefs"));
    assert_eq!(build.tool.as_deref(), Some("/opt/mklittlefs"));
    // The upload's `--image` is the build's `--output`: one path, both halves.
    assert_eq!(build.output.as_deref(), Some("out/fs.bin"));
    assert!(!build.check_only, "uploadfs always wants a real image");
}

/// ESP32 is the only platform fbuild builds filesystem images for.
#[test]
fn require_flash_filesystem_accepts_espressif32() {
    assert!(require_flash_filesystem(Some("espressif32"), "esp32s3").is_ok());
}

/// AVR and Teensy have no flashable filesystem from a build tool; the error is
/// the whole point (the issue asks for a clear failure, never a silent no-op).
#[test]
fn require_flash_filesystem_rejects_platforms_without_one() {
    for platform in ["atmelavr", "teensy"] {
        let error = require_flash_filesystem(Some(platform), "uno")
            .expect_err("platform without a flash filesystem must be rejected")
            .to_string();
        assert!(
            error.contains("does not build filesystem images"),
            "unexpected message for {platform}: {error}"
        );
    }
    // ESP8266 has a flash filesystem upstream, but fbuild builds ESP32
    // partition tables — still a refusal, still a clear one.
    let esp8266 = require_flash_filesystem(Some("espressif8266"), "nodemcuv2")
        .expect_err("esp8266 is not implemented")
        .to_string();
    assert!(esp8266.contains("ESP8266 is not implemented"), "{esp8266}");
}

#[test]
fn require_flash_filesystem_rejects_a_missing_platform() {
    let error = require_flash_filesystem(None, "mystery")
        .expect_err("a missing platform cannot be flashed")
        .to_string();
    assert!(error.contains("needs an ESP32 environment"), "{error}");
}

/// Port selection must never guess: one port is unambiguous, zero and many are
/// errors that name the candidates.
#[test]
fn choose_port_picks_the_only_detected_port() {
    let chosen = choose_port(&["/dev/ttyACM0".to_string()]).expect("single port");
    assert_eq!(chosen, "/dev/ttyACM0");
}

#[test]
fn choose_port_refuses_to_guess_between_ports() {
    let none = choose_port(&[]).expect_err("no ports").to_string();
    assert!(none.contains("pass --port"), "{none}");

    let many = choose_port(&["/dev/ttyACM0".to_string(), "/dev/ttyUSB0".to_string()])
        .expect_err("ambiguous")
        .to_string();
    assert!(many.contains("2 serial ports detected"), "{many}");
    assert!(many.contains("/dev/ttyACM0, /dev/ttyUSB0"), "{many}");
}

/// `--image` is resolved against the project, like every other path flag.
#[test]
fn absolute_resolves_relative_paths_against_the_project() {
    let project = Path::new("/work/blink");
    assert_eq!(
        absolute(project, "build/littlefs.bin")
            .as_path()
            .display()
            .to_string(),
        "/work/blink/build/littlefs.bin"
    );
    assert_eq!(
        absolute(project, "/tmp/fs.bin")
            .as_path()
            .display()
            .to_string(),
        "/tmp/fs.bin"
    );
}
