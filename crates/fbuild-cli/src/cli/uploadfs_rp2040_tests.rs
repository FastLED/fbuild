//! Unit tests for the RP2040 half of `fbuild uploadfs` (FastLED/fbuild#1358).

use super::*;

/// The only mounted Pico volume is the one to flash.
#[test]
fn choose_volume_accepts_the_only_volume() {
    let volumes = vec!["/media/user/RPI-RP2".to_string()];
    assert_eq!(choose_volume(&volumes).unwrap(), "/media/user/RPI-RP2");
}

/// No mounted volume is an actionable error naming the BOOTSEL step, never a
/// silent no-op.
#[test]
fn choose_volume_explains_a_missing_volume() {
    let error = choose_volume(&[])
        .expect_err("no volume cannot be flashed")
        .to_string();
    assert!(error.contains("no Pico BOOTSEL volume detected"), "{error}");
    assert!(error.contains("BOOTSEL"), "{error}");
}

/// Two mounted Picos are ambiguous: flashing either one could hit the wrong
/// board, so both paths are named and nothing is written.
#[test]
fn choose_volume_refuses_an_ambiguous_set() {
    let volumes = vec![
        "/media/user/RPI-RP2".to_string(),
        "/run/media/user/RPI-RP2".to_string(),
    ];
    let error = choose_volume(&volumes)
        .expect_err("two volumes are ambiguous")
        .to_string();
    assert!(error.contains("2 Pico BOOTSEL volumes detected"), "{error}");
    assert!(error.contains("/media/user/RPI-RP2"), "{error}");
    assert!(error.contains("/run/media/user/RPI-RP2"), "{error}");
}

/// `fs_start` is already the XIP address the firmware mounts; subtracting the
/// flash mapping would relocate the filesystem under the firmware.
#[test]
fn uf2_base_address_is_the_declared_flash_address_unchanged() {
    assert_eq!(uf2_base_address(0x1017_f000), 0x1017_f000);
    assert_eq!(uf2_base_address(0x1000_0000), 0x1000_0000);
}

/// `--verify` is rejected for RP2040 rather than silently downgraded.
#[test]
fn require_verify_supported_rejects_the_flag() {
    assert!(require_verify_supported(false).is_ok());
    let error = require_verify_supported(true)
        .expect_err("--verify is not available for RP2040")
        .to_string();
    assert!(error.contains("--verify"), "{error}");
    assert!(error.contains("PICOBOOT"), "{error}");
}
