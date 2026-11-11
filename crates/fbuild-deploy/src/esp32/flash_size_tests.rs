//! Tests for the declared-vs-detected flash-size preflight
//! (FastLED/fbuild#1992).
//!
//! The defect: `fbuild deploy` on an 8 MB board JSON over a 4 MB ESP32-C6FH4
//! flashed and verified, printed `deploy succeeded`, and the app then panicked
//! in `spi_flash` because the image header declared twice the flash the chip
//! has. These tests pin the parser, the verdict, the argv, the message and the
//! refusal itself (through a fake `esptool` that answers `flash-id`).

#![cfg(test)]

use std::path::Path;
use std::time::Duration;

use fbuild_core::path::NormalizedPath;

use super::deployer::Esp32Deployer;
use super::flash_size::{
    FlashSizeVerdict, build_flash_id_args, describe_flash_size, flash_size_verdict,
    mismatch_message,
};
use super::parse::parse_detected_flash_size;
use super::tests::test_esptool_params;
use crate::Deployer;

const MB: u64 = 1024 * 1024;

/// The real `esptool flash-id` tail captured from an ESP32-C6FH4 on
/// 2026-11-11 (the run that exposed the defect).
const REAL_FLASH_ID: &str = "\
esptool v5.4.0
Uploading stub flasher...
Running stub flasher...
Stub flasher running.

Flash Memory Information:
=========================
Manufacturer: 20
Device: 4016
Detected flash size: 4MB

Hard resetting via RTS pin...
";

#[test]
fn parses_the_detected_size_out_of_real_flash_id_output() {
    assert_eq!(parse_detected_flash_size(REAL_FLASH_ID), Some(4 * MB));
}

#[test]
fn parses_kilobyte_labels_and_ignores_unrelated_lines() {
    assert_eq!(
        parse_detected_flash_size("Connecting...\nDetected flash size: 512KB\n"),
        Some(512 * 1024)
    );
    assert_eq!(parse_detected_flash_size("esptool v5.4.0\nDone.\n"), None);
    assert_eq!(parse_detected_flash_size(""), None);
}

#[test]
fn verdict_flags_only_a_declared_size_larger_than_the_chip() {
    assert_eq!(
        flash_size_verdict(Some(8 * MB), Some(4 * MB)),
        FlashSizeVerdict::Exceeds {
            declared: 8 * MB,
            detected: 4 * MB
        }
    );
    assert_eq!(
        flash_size_verdict(Some(4 * MB), Some(4 * MB)),
        FlashSizeVerdict::Fits
    );
    // A board JSON that under-declares the chip still boots, so it is not a
    // refusal: flashing it just wastes flash.
    assert_eq!(
        flash_size_verdict(Some(4 * MB), Some(8 * MB)),
        FlashSizeVerdict::Fits
    );
    assert_eq!(
        flash_size_verdict(None, Some(4 * MB)),
        FlashSizeVerdict::Unknown
    );
    assert_eq!(
        flash_size_verdict(Some(4 * MB), None),
        FlashSizeVerdict::Unknown
    );
}

#[test]
fn flash_id_argv_carries_the_resolved_tool_chip_and_port() {
    let esptool = vec!["/usr/bin/esptool".to_string()];
    assert_eq!(
        build_flash_id_args(&esptool, "esp32c6", "/dev/ttyACM0"),
        vec![
            "/usr/bin/esptool".to_string(),
            "--chip".to_string(),
            "esp32c6".to_string(),
            "--port".to_string(),
            "/dev/ttyACM0".to_string(),
            "flash-id".to_string(),
        ]
    );
}

#[test]
fn message_names_both_sizes_the_knob_and_the_boot_failure() {
    let msg = mismatch_message("esp32c6", 8 * MB, 4 * MB);
    assert!(msg.contains("4MB"), "{msg}");
    assert!(msg.contains("8MB"), "{msg}");
    assert!(
        msg.contains("board_upload.flash_size = 4MB"),
        "the fix must be spelled out: {msg}"
    );
    assert!(
        msg.contains("__esp_system_init_fn_init_flash"),
        "the prevented failure must be named: {msg}"
    );
    assert!(
        msg.contains("FBUILD_SKIP_FLASH_SIZE_PREFLIGHT"),
        "the escape hatch must be discoverable: {msg}"
    );
    assert_eq!(describe_flash_size(8 * MB), "8MB");
    assert_eq!(describe_flash_size(512 * 1024), "512KB");
}

/// Install an executable shell script without ever exec'ing a file that is
/// still open for writing (a sibling test thread forking mid-write inherits
/// the write fd and exec then fails with ETXTBSY).
fn write_script(path: &Path, body: &str) {
    let staging = path.with_extension("staging");
    std::fs::write(&staging, body).unwrap();
    fbuild_core::platform::fs::set_executable(&staging).unwrap();
    std::fs::rename(&staging, path).unwrap();
}

/// A fake `esptool` that answers `flash-id` with `detected` and otherwise
/// behaves like a successful full flash.
fn fake_esptool(dir: &Path, detected: &str) -> NormalizedPath {
    let script = dir.join("esptool");
    write_script(
        &script,
        &format!(
            "#!/bin/sh\n\
             case \" $* \" in\n\
               *\" flash-id \"*) echo 'Detected flash size: {detected}'; exit 0;;\n\
             esac\n\
             echo 'Hash of data verified.'\n\
             exit 0\n"
        ),
    );
    NormalizedPath::new(script)
}

fn deployer(esptool: &NormalizedPath, flash_size: &str) -> Esp32Deployer {
    let params = test_esptool_params();
    let mut deployer = Esp32Deployer::new(
        "esp32c6", "460800", "0x0", "0x8000", "0x10000", &params, false,
    )
    .with_esptool_path(Some(esptool.clone()))
    .with_flash_timeout(Duration::from_secs(30));
    deployer.flash_size = flash_size.to_string();
    deployer
}

/// The regression: an 8 MB declaration over a 4 MB chip used to flash, verify
/// and report success, producing a device that panics at boot. It must now be
/// refused before anything is written.
#[tokio::test]
async fn refuses_to_flash_an_image_header_larger_than_the_chip() {
    if fbuild_core::platform::host::is_windows() {
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let esptool = fake_esptool(tmp.path(), "4MB");
    let firmware = NormalizedPath::new(tmp.path().join("firmware.bin"));
    std::fs::write(firmware.as_path(), [0u8; 32]).unwrap();

    let err = deployer(&esptool, "8MB")
        .deploy(tmp.path(), "default", firmware.as_path(), Some("COM13"))
        .await
        .expect_err("an image header bigger than the chip must not be flashed");

    let message = err.to_string();
    assert!(
        message.contains("board_upload.flash_size = 4MB"),
        "{message}"
    );
    assert!(message.contains("panics at boot"), "{message}");
}

/// Control: the same fake tool reporting a chip that matches the declaration
/// still flashes, so the preflight cannot wedge a working deploy.
#[tokio::test]
async fn flashes_when_the_chip_matches_the_declaration() {
    if fbuild_core::platform::host::is_windows() {
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let esptool = fake_esptool(tmp.path(), "8MB");
    let firmware = NormalizedPath::new(tmp.path().join("firmware.bin"));
    std::fs::write(firmware.as_path(), [0u8; 32]).unwrap();

    let result = deployer(&esptool, "8MB")
        .deploy(tmp.path(), "default", firmware.as_path(), Some("COM13"))
        .await
        .expect("a matching chip must not be refused");

    assert!(result.success, "{}", result.message);
    assert!(result.stdout.contains("Hash of data verified."));
}

/// A tool that cannot answer `flash-id` (older esptool, wedged port) leaves
/// the deploy untouched: the diagnostic never blocks a flash on its own.
#[tokio::test]
async fn unparseable_probe_leaves_the_flash_unchanged() {
    if fbuild_core::platform::host::is_windows() {
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let esptool = fake_esptool(tmp.path(), "unknown");
    let firmware = NormalizedPath::new(tmp.path().join("firmware.bin"));
    std::fs::write(firmware.as_path(), [0u8; 32]).unwrap();

    let result = deployer(&esptool, "8MB")
        .deploy(tmp.path(), "default", firmware.as_path(), Some("COM13"))
        .await
        .expect("an unknown detected size must not fail the deploy");

    assert!(result.success, "{}", result.message);
}
