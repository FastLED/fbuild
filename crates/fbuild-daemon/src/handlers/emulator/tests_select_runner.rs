//! Tests for `select_runner` and the `is_qemu_supported_esp32_mcu` helper.

use super::qemu_deploy::is_qemu_supported_esp32_mcu;
use super::runners::SimavrRunner;
use super::select::select_runner;
use std::collections::HashMap;
use std::path::Path;

// -----------------------------------------------------------------------
// select_runner tests for simavr
// -----------------------------------------------------------------------

#[test]
fn select_runner_explicit_simavr_for_uno() {
    let result = select_runner(
        Path::new("/tmp/test"),
        "uno",
        fbuild_core::Platform::AtmelAvr,
        "uno",
        &HashMap::new(),
        Some("simavr"),
    );
    assert!(result.is_ok(), "select_runner should accept simavr for uno");
    assert_eq!(result.unwrap().name(), "simavr");
}

#[test]
fn select_runner_explicit_simavr_for_mega() {
    let result = select_runner(
        Path::new("/tmp/test"),
        "megaatmega2560",
        fbuild_core::Platform::AtmelAvr,
        "megaatmega2560",
        &HashMap::new(),
        Some("simavr"),
    );
    assert!(
        result.is_ok(),
        "select_runner should accept simavr for mega"
    );
    assert_eq!(result.unwrap().name(), "simavr");
}

#[test]
fn select_runner_explicit_simavr_for_leonardo() {
    let result = select_runner(
        Path::new("/tmp/test"),
        "leonardo",
        fbuild_core::Platform::AtmelAvr,
        "leonardo",
        &HashMap::new(),
        Some("simavr"),
    );
    assert!(
        result.is_ok(),
        "select_runner should accept simavr for leonardo"
    );
    assert_eq!(result.unwrap().name(), "simavr");
}

#[test]
fn select_runner_explicit_simavr_rejects_esp32() {
    let result = select_runner(
        Path::new("/tmp/test"),
        "esp32dev",
        fbuild_core::Platform::Espressif32,
        "esp32dev",
        &HashMap::new(),
        Some("simavr"),
    );
    assert!(result.is_err(), "simavr should reject ESP32 boards");
}

#[test]
fn select_runner_auto_detects_simavr_for_mega() {
    // ATmega2560 should auto-detect simavr since it's not ATmega328P
    let result = select_runner(
        Path::new("/tmp/test"),
        "megaatmega2560",
        fbuild_core::Platform::AtmelAvr,
        "megaatmega2560",
        &HashMap::new(),
        None,
    );
    assert!(
        result.is_ok(),
        "auto-detect should find simavr for mega: {:?}",
        result.err()
    );
    assert_eq!(result.unwrap().name(), "simavr");
}

#[test]
fn select_runner_auto_detects_simavr_for_leonardo() {
    let result = select_runner(
        Path::new("/tmp/test"),
        "leonardo",
        fbuild_core::Platform::AtmelAvr,
        "leonardo",
        &HashMap::new(),
        None,
    );
    assert!(
        result.is_ok(),
        "auto-detect should find simavr for leonardo: {:?}",
        result.err()
    );
    assert_eq!(result.unwrap().name(), "simavr");
}

#[test]
fn select_runner_auto_detects_avr8js_for_uno() {
    // ATmega328P should still default to avr8js, not simavr
    let result = select_runner(
        Path::new("/tmp/test"),
        "uno",
        fbuild_core::Platform::AtmelAvr,
        "uno",
        &HashMap::new(),
        None,
    );
    assert!(result.is_ok());
    assert_eq!(
        result.unwrap().name(),
        "avr8js ATmega328P",
        "ATmega328P should default to avr8js"
    );
}

#[test]
fn select_runner_simavr_name_matches() {
    use super::runners::EmulatorRunner;
    let board = fbuild_test_support::board_for_test("megaatmega2560");
    let runner = SimavrRunner::new(board);
    assert_eq!(runner.name(), "simavr");
}

// -----------------------------------------------------------------------
// select_runner tests for ESP32 QEMU (Issue #25)
// -----------------------------------------------------------------------

#[test]
fn select_runner_explicit_qemu_for_esp32dev() {
    let result = select_runner(
        Path::new("/tmp/test"),
        "esp32dev",
        fbuild_core::Platform::Espressif32,
        "esp32dev",
        &HashMap::new(),
        Some("qemu"),
    );
    assert!(
        result.is_ok(),
        "select_runner should accept qemu for esp32dev: {:?}",
        result.err()
    );
    assert_eq!(result.unwrap().name(), "QEMU ESP32");
}

#[test]
fn select_runner_explicit_qemu_for_esp32s3() {
    let result = select_runner(
        Path::new("/tmp/test"),
        "esp32-s3-devkitc-1",
        fbuild_core::Platform::Espressif32,
        "esp32-s3-devkitc-1",
        &HashMap::new(),
        Some("qemu"),
    );
    assert!(
        result.is_ok(),
        "select_runner should accept qemu for esp32-s3: {:?}",
        result.err()
    );
    assert_eq!(result.unwrap().name(), "QEMU ESP32S3");
}

#[test]
fn select_runner_explicit_qemu_accepts_esp32c3() {
    let result = select_runner(
        Path::new("/tmp/test"),
        "esp32-c3-devkitm-1",
        fbuild_core::Platform::Espressif32,
        "esp32-c3-devkitm-1",
        &HashMap::new(),
        Some("qemu"),
    );
    assert!(
        result.is_ok(),
        "QEMU should accept ESP32-C3 via qemu-system-riscv32: {:?}",
        result.err()
    );
}

#[test]
fn select_runner_explicit_qemu_accepts_esp32c6() {
    let result = select_runner(
        Path::new("/tmp/test"),
        "esp32-c6-devkitc-1",
        fbuild_core::Platform::Espressif32,
        "esp32-c6-devkitc-1",
        &HashMap::new(),
        Some("qemu"),
    );
    assert!(
        result.is_ok(),
        "QEMU should accept ESP32-C6 via qemu-system-riscv32: {:?}",
        result.err()
    );
}

#[test]
fn select_runner_explicit_qemu_accepts_esp32h2() {
    let result = select_runner(
        Path::new("/tmp/test"),
        "esp32-h2-devkitm-1",
        fbuild_core::Platform::Espressif32,
        "esp32-h2-devkitm-1",
        &HashMap::new(),
        Some("qemu"),
    );
    assert!(
        result.is_ok(),
        "QEMU should accept ESP32-H2 via qemu-system-riscv32: {:?}",
        result.err()
    );
}

#[test]
fn select_runner_explicit_qemu_rejects_esp32s2() {
    let result = select_runner(
        Path::new("/tmp/test"),
        "esp32-s2-saola-1",
        fbuild_core::Platform::Espressif32,
        "esp32-s2-saola-1",
        &HashMap::new(),
        Some("qemu"),
    );
    assert!(
        result.is_err(),
        "QEMU should reject ESP32-S2 (not emulated upstream)"
    );
}

#[test]
fn select_runner_auto_detects_qemu_for_esp32dev() {
    let result = select_runner(
        Path::new("/tmp/test"),
        "esp32dev",
        fbuild_core::Platform::Espressif32,
        "esp32dev",
        &HashMap::new(),
        None,
    );
    assert!(
        result.is_ok(),
        "auto-detect should find qemu for esp32dev: {:?}",
        result.err()
    );
    assert_eq!(result.unwrap().name(), "QEMU ESP32");
}

#[test]
fn select_runner_auto_detects_qemu_for_esp32s3() {
    let result = select_runner(
        Path::new("/tmp/test"),
        "esp32-s3-devkitc-1",
        fbuild_core::Platform::Espressif32,
        "esp32-s3-devkitc-1",
        &HashMap::new(),
        None,
    );
    assert!(
        result.is_ok(),
        "auto-detect should find qemu for esp32-s3: {:?}",
        result.err()
    );
    assert_eq!(result.unwrap().name(), "QEMU ESP32S3");
}

#[test]
fn is_qemu_supported_esp32_mcu_accepts_xtensa() {
    assert!(is_qemu_supported_esp32_mcu("esp32"));
    assert!(is_qemu_supported_esp32_mcu("ESP32"));
    assert!(is_qemu_supported_esp32_mcu("esp32s3"));
    assert!(is_qemu_supported_esp32_mcu("ESP32S3"));
}

#[test]
fn is_qemu_supported_esp32_mcu_accepts_riscv() {
    assert!(is_qemu_supported_esp32_mcu("esp32c3"));
    assert!(is_qemu_supported_esp32_mcu("ESP32C3"));
    assert!(is_qemu_supported_esp32_mcu("esp32c6"));
    assert!(is_qemu_supported_esp32_mcu("esp32h2"));
}

#[test]
fn is_qemu_supported_esp32_mcu_rejects_unsupported() {
    assert!(!is_qemu_supported_esp32_mcu("esp32s2"));
    assert!(!is_qemu_supported_esp32_mcu("atmega328p"));
    assert!(!is_qemu_supported_esp32_mcu("esp32p4"));
}
