//! Unit tests for the ESP32 orchestrator's helpers and public API.

use super::cdc::{cdc_on_boot_enabled, is_esp32_project, warn_if_cdc_on_boot};
use super::helpers::{
    framework_failure_marker, framework_signature, record_failed_framework_lib,
    should_skip_failed_framework_lib,
};
use super::Esp32Orchestrator;
use crate::BuildOrchestrator;
use fbuild_core::Platform;
use std::path::PathBuf;
use std::time::Duration;

#[test]
fn test_esp32_orchestrator_platform() {
    let orch = Esp32Orchestrator;
    assert_eq!(orch.platform(), Platform::Espressif32);
}

#[test]
fn test_is_esp32_project() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("platformio.ini"),
        "[env:esp32c6]\nplatform = espressif32\nboard = esp32-c6\nframework = arduino\n",
    )
    .unwrap();
    assert!(is_esp32_project(tmp.path(), "esp32c6"));
    assert!(!is_esp32_project(tmp.path(), "uno"));
}

#[test]
fn test_is_not_esp32_project() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("platformio.ini"),
        "[env:uno]\nplatform = atmelavr\nboard = uno\nframework = arduino\n",
    )
    .unwrap();
    assert!(!is_esp32_project(tmp.path(), "uno"));
}

// --- CDC on boot warning tests ---

/// Board that enables CDC on boot via extra_flags (e.g. Adafruit Feather ESP32-S3).
#[test]
fn test_cdc_enabled_by_board_extra_flags() {
    let board_flags = Some(
        "-DARDUINO_ADAFRUIT_FEATHER_ESP32S3 -DARDUINO_USB_CDC_ON_BOOT=1 -DARDUINO_RUNNING_CORE=1",
    );
    assert!(cdc_on_boot_enabled(board_flags, &[]));
}

/// Board that explicitly disables CDC on boot.
#[test]
fn test_cdc_disabled_by_board_extra_flags() {
    let board_flags = Some("-DARDUINO_FREENOVE_ESP32_S3_WROOM -DARDUINO_USB_CDC_ON_BOOT=0");
    assert!(!cdc_on_boot_enabled(board_flags, &[]));
}

/// Plain ESP32 dev board with no CDC flag at all — not enabled.
#[test]
fn test_no_cdc_flag_returns_false() {
    let board_flags = Some("-DARDUINO_ESP32_DEV");
    assert!(!cdc_on_boot_enabled(board_flags, &[]));
}

/// No board flags at all — not enabled.
#[test]
fn test_no_flags_at_all_returns_false() {
    assert!(!cdc_on_boot_enabled(None, &[]));
}

/// User build_flags override a board-level enable (last definition wins).
#[test]
fn test_user_flag_overrides_board_enable() {
    let board_flags = Some("-DARDUINO_USB_CDC_ON_BOOT=1");
    let user_flags = vec!["-DARDUINO_USB_CDC_ON_BOOT=0".to_string()];
    assert!(!cdc_on_boot_enabled(board_flags, &user_flags));
}

/// User build_flags can enable CDC that the board left unconfigured.
#[test]
fn test_user_flag_enables_cdc() {
    let board_flags = Some("-DARDUINO_ESP32_DEV");
    let user_flags = vec!["-DARDUINO_USB_CDC_ON_BOOT=1".to_string()];
    assert!(cdc_on_boot_enabled(board_flags, &user_flags));
}

/// Multiple user flags — last one wins.
#[test]
fn test_last_user_flag_wins() {
    let board_flags = Some("-DARDUINO_USB_CDC_ON_BOOT=1");
    let user_flags = vec![
        "-DARDUINO_USB_CDC_ON_BOOT=0".to_string(),
        "-DARDUINO_USB_CDC_ON_BOOT=1".to_string(),
    ];
    assert!(cdc_on_boot_enabled(board_flags, &user_flags));
}

/// Flags provided as whitespace-separated string should be parsed correctly.
#[test]
fn test_multi_flag_string_parsed_correctly() {
    // Board flags: the enable flag appears after another flag.
    let board_flags = Some("-DSOME_DEFINE -DARDUINO_USB_CDC_ON_BOOT=1 -DANOTHER=1");
    assert!(cdc_on_boot_enabled(board_flags, &[]));
}

/// `warn_if_cdc_on_boot` should not panic for any combination of inputs.
#[test]
fn test_warn_if_cdc_on_boot_no_panic() {
    // CDC enabled — triggers warning path
    warn_if_cdc_on_boot(
        "Adafruit Feather ESP32-S3",
        Some("-DARDUINO_USB_CDC_ON_BOOT=1"),
        &[],
    );
    // CDC disabled — no warning
    warn_if_cdc_on_boot(
        "Freenove ESP32-S3-WROOM",
        Some("-DARDUINO_USB_CDC_ON_BOOT=0"),
        &[],
    );
    // No flag at all — no warning
    warn_if_cdc_on_boot("ESP32 Dev Module", Some("-DARDUINO_ESP32_DEV"), &[]);
    // No board flags — no warning
    warn_if_cdc_on_boot("Some Board", None, &[]);
    // User override suppresses board enable
    warn_if_cdc_on_boot(
        "Some Board",
        Some("-DARDUINO_USB_CDC_ON_BOOT=1"),
        &["-DARDUINO_USB_CDC_ON_BOOT=0".to_string()],
    );
}

#[test]
fn test_framework_signature_changes_with_flags() {
    let includes = vec![PathBuf::from("C:/sdk/include")];
    let sig_a = framework_signature(
        &includes,
        &["-O2".to_string()],
        &["-std=gnu++17".to_string()],
    );
    let sig_b = framework_signature(
        &includes,
        &["-O0".to_string()],
        &["-std=gnu++17".to_string()],
    );
    assert_ne!(sig_a, sig_b);
}

#[test]
fn test_skip_failed_framework_lib_when_marker_matches_and_is_current() {
    let tmp = tempfile::TempDir::new().unwrap();
    let source = tmp.path().join("Matter.cpp");
    std::fs::write(&source, "int x;").unwrap();
    let marker = framework_failure_marker(tmp.path(), "matter");
    let sig = framework_signature(&[], &["-O2".to_string()], &["-std=gnu++2b".to_string()]);
    std::thread::sleep(Duration::from_millis(20));
    record_failed_framework_lib(&marker, &sig, "compile failed");

    assert!(should_skip_failed_framework_lib(&marker, &sig, &[source]).unwrap());
}

#[test]
fn test_retry_failed_framework_lib_after_source_change() {
    let tmp = tempfile::TempDir::new().unwrap();
    let source = tmp.path().join("Matter.cpp");
    std::fs::write(&source, "int x;").unwrap();
    let marker = framework_failure_marker(tmp.path(), "matter");
    let sig = framework_signature(&[], &["-O2".to_string()], &["-std=gnu++2b".to_string()]);
    std::thread::sleep(Duration::from_millis(20));
    record_failed_framework_lib(&marker, &sig, "compile failed");
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(&source, "int y;").unwrap();

    assert!(!should_skip_failed_framework_lib(&marker, &sig, &[source]).unwrap());
}

#[test]
fn test_retry_failed_framework_lib_after_signature_change() {
    let tmp = tempfile::TempDir::new().unwrap();
    let source = tmp.path().join("Matter.cpp");
    std::fs::write(&source, "int x;").unwrap();
    let marker = framework_failure_marker(tmp.path(), "matter");
    let sig_a = framework_signature(&[], &["-O2".to_string()], &["-std=gnu++2b".to_string()]);
    let sig_b = framework_signature(&[], &["-O0".to_string()], &["-std=gnu++2b".to_string()]);
    std::thread::sleep(Duration::from_millis(20));
    record_failed_framework_lib(&marker, &sig_a, "compile failed");

    assert!(!should_skip_failed_framework_lib(&marker, &sig_b, &[source]).unwrap());
}
