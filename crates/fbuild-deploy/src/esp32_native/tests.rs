//! Unit tests for the `esp32_native` module tree. Kept in a single
//! file so a refactor that splits the module doesn't fan tests out
//! into multiple test binaries.

use espflash::connection::{ResetAfterOperation, ResetBeforeOperation};

use crate::esp32::{FlashRegion, RegionVerifyResult};
use crate::DeployOutcome;

use super::progress::LoggingProgressBridge;
use super::transport::{
    local_md5, parse_after_reset, parse_before_reset, parse_chip, region_name, render_native_stdout,
};
use super::types::NativeWriteRegion;
use super::verify::collect_standard_regions;
use super::write::{
    collect_selected_write_regions, collect_standard_write_regions, outcome_for,
    try_write_deployment_native,
};

#[test]
fn parse_chip_accepts_lowercase_family_members() {
    // All ten currently-supported chips must round-trip through
    // our wrapper. Guards against a future espflash bump that
    // renames a variant.
    for name in [
        "esp32", "esp32c2", "esp32c3", "esp32c5", "esp32c6", "esp32c61", "esp32h2", "esp32p4",
        "esp32s2", "esp32s3",
    ] {
        parse_chip(name)
            .unwrap_or_else(|e| panic!("chip '{}' must parse in espflash: {:?}", name, e));
    }
}

#[test]
fn parse_chip_accepts_uppercase_because_we_lowercase_first() {
    parse_chip("ESP32S3").unwrap();
    parse_chip("Esp32C6").unwrap();
}

#[test]
fn parse_chip_rejects_unknown() {
    assert!(parse_chip("esp99").is_err());
}

#[test]
fn parse_before_reset_covers_esptool_spellings() {
    assert!(matches!(
        parse_before_reset("default-reset").unwrap(),
        ResetBeforeOperation::DefaultReset
    ));
    assert!(matches!(
        parse_before_reset("no-reset").unwrap(),
        ResetBeforeOperation::NoReset
    ));
    assert!(matches!(
        parse_before_reset("usb-reset").unwrap(),
        ResetBeforeOperation::UsbReset
    ));
    assert!(parse_before_reset("bogus").is_err());
}

#[test]
fn parse_after_reset_covers_esptool_spellings() {
    assert!(matches!(
        parse_after_reset("hard-reset").unwrap(),
        ResetAfterOperation::HardReset
    ));
    assert!(matches!(
        parse_after_reset("no-reset").unwrap(),
        ResetAfterOperation::NoReset
    ));
    assert!(parse_after_reset("bogus").is_err());
}

#[test]
fn local_md5_matches_known_vector() {
    // RFC 1321 test vector: MD5("") = d41d8cd98f00b204e9800998ecf8427e
    let empty = local_md5(b"");
    let expected_bytes: [u8; 16] = [
        0xd4, 0x1d, 0x8c, 0xd9, 0x8f, 0x00, 0xb2, 0x04, 0xe9, 0x80, 0x09, 0x98, 0xec, 0xf8, 0x42,
        0x7e,
    ];
    assert_eq!(empty, u128::from_le_bytes(expected_bytes));

    // MD5("abc") = 900150983cd24fb0d6963f7d28e17f72
    let abc = local_md5(b"abc");
    let expected_bytes: [u8; 16] = [
        0x90, 0x01, 0x50, 0x98, 0x3c, 0xd2, 0x4f, 0xb0, 0xd6, 0x96, 0x3f, 0x7d, 0x28, 0xe1, 0x7f,
        0x72,
    ];
    assert_eq!(abc, u128::from_le_bytes(expected_bytes));
}

#[test]
fn collect_standard_regions_skips_missing_optional_files() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fw = tmp.path().join("firmware.bin");
    std::fs::write(&fw, b"firmware").unwrap();

    let regions = collect_standard_regions(&fw, 0x0, 0x8000, 0x10000);
    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].region, FlashRegion::Firmware);
    assert_eq!(regions[0].offset, 0x10000);
}

#[test]
fn collect_standard_regions_includes_optional_files_when_present() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fw = tmp.path().join("firmware.bin");
    std::fs::write(&fw, b"firmware").unwrap();
    std::fs::write(tmp.path().join("bootloader.bin"), b"boot").unwrap();
    std::fs::write(tmp.path().join("partitions.bin"), b"part").unwrap();

    let regions = collect_standard_regions(&fw, 0x0, 0x8000, 0x10000);
    assert_eq!(regions.len(), 3);
    assert_eq!(regions[0].region, FlashRegion::Bootloader);
    assert_eq!(regions[0].offset, 0x0);
    assert_eq!(regions[1].region, FlashRegion::Partitions);
    assert_eq!(regions[1].offset, 0x8000);
    assert_eq!(regions[2].region, FlashRegion::Firmware);
    assert_eq!(regions[2].offset, 0x10000);
}

#[test]
fn render_native_stdout_mentions_all_regions() {
    let results = vec![
        RegionVerifyResult {
            region: FlashRegion::Bootloader,
            matched: true,
        },
        RegionVerifyResult {
            region: FlashRegion::Firmware,
            matched: false,
        },
    ];
    let out = render_native_stdout(&results);
    assert!(out.contains("bootloader"));
    assert!(out.contains("firmware"));
    assert!(out.contains("digest matched"));
    assert!(out.contains("digest mismatch"));
}

// --- native write-flash tests (issue #66 PR #89 follow-up) ---
//
// Most of the write flow is live hardware code. What we can test
// without a board attached is the region assembly, the
// DeployOutcome mapping, and the progress-bridge throttling — the
// three pure pieces where a regression would silently corrupt the
// daemon's deploy response.

#[test]
fn collect_standard_write_regions_skips_missing_optional_files() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fw = tmp.path().join("firmware.bin");
    std::fs::write(&fw, b"firmware").unwrap();

    let regions = collect_standard_write_regions(&fw, 0x0, 0x8000, 0x10000);
    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].region, FlashRegion::Firmware);
    assert_eq!(regions[0].offset, 0x10000);
}

#[test]
fn collect_standard_write_regions_includes_optional_files_when_present() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fw = tmp.path().join("firmware.bin");
    std::fs::write(&fw, b"firmware").unwrap();
    std::fs::write(tmp.path().join("bootloader.bin"), b"boot").unwrap();
    std::fs::write(tmp.path().join("partitions.bin"), b"part").unwrap();

    let regions = collect_standard_write_regions(&fw, 0x0, 0x8000, 0x10000);
    assert_eq!(regions.len(), 3);
    assert_eq!(regions[0].region, FlashRegion::Bootloader);
    assert_eq!(regions[0].offset, 0x0);
    assert_eq!(regions[1].region, FlashRegion::Partitions);
    assert_eq!(regions[1].offset, 0x8000);
    assert_eq!(regions[2].region, FlashRegion::Firmware);
    assert_eq!(regions[2].offset, 0x10000);
}

#[test]
fn collect_selected_write_regions_errors_on_empty_request() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fw = tmp.path().join("firmware.bin");
    std::fs::write(&fw, b"firmware").unwrap();
    let err = collect_selected_write_regions(&fw, 0x0, 0x8000, 0x10000, &[]).unwrap_err();
    assert!(err.to_string().contains("empty request"));
}

#[test]
fn collect_selected_write_regions_errors_when_file_missing() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fw = tmp.path().join("firmware.bin");
    std::fs::write(&fw, b"firmware").unwrap();
    // No bootloader.bin on disk.
    let err = collect_selected_write_regions(&fw, 0x0, 0x8000, 0x10000, &[FlashRegion::Bootloader])
        .unwrap_err();
    assert!(err.to_string().contains("missing"));
}

#[test]
fn collect_selected_write_regions_returns_requested_subset() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fw = tmp.path().join("firmware.bin");
    std::fs::write(&fw, b"firmware").unwrap();
    std::fs::write(tmp.path().join("bootloader.bin"), b"boot").unwrap();
    std::fs::write(tmp.path().join("partitions.bin"), b"part").unwrap();

    let out = collect_selected_write_regions(
        &fw,
        0x0,
        0x8000,
        0x10000,
        &[FlashRegion::Firmware, FlashRegion::Bootloader],
    )
    .unwrap();
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].region, FlashRegion::Firmware);
    assert_eq!(out[0].offset, 0x10000);
    assert_eq!(out[1].region, FlashRegion::Bootloader);
    assert_eq!(out[1].offset, 0x0);
}

#[test]
fn outcome_for_full_write_reports_full_flash() {
    let regions = vec![NativeWriteRegion {
        region: FlashRegion::Firmware,
        offset: 0x10000,
        path: std::path::PathBuf::from("firmware.bin"),
    }];
    assert!(matches!(
        outcome_for(false, &regions),
        DeployOutcome::FullFlash
    ));
}

#[test]
fn outcome_for_selective_write_carries_region_list() {
    let regions = vec![
        NativeWriteRegion {
            region: FlashRegion::Bootloader,
            offset: 0x0,
            path: std::path::PathBuf::from("bootloader.bin"),
        },
        NativeWriteRegion {
            region: FlashRegion::Firmware,
            offset: 0x10000,
            path: std::path::PathBuf::from("firmware.bin"),
        },
    ];
    match outcome_for(true, &regions) {
        DeployOutcome::SelectiveFlash { regions } => {
            assert_eq!(
                regions,
                vec![FlashRegion::Bootloader, FlashRegion::Firmware]
            );
        }
        other => panic!("expected SelectiveFlash, got {:?}", other),
    }
}

#[test]
fn try_write_deployment_native_rejects_empty_regions() {
    let err = try_write_deployment_native(
        "esp32s3",
        "COM99",
        460_800,
        "default-reset",
        "hard-reset",
        &[],
        false,
    )
    .unwrap_err();
    assert!(err.to_string().contains("no regions"));
}

#[test]
fn progress_bridge_throttles_updates_to_ten_percent_boundaries() {
    use espflash::target::ProgressCallbacks;
    // Assert the 10%-boundary throttle actually fires — without it
    // a 1 MB write spams hundreds of log lines per region.
    let mut bridge = LoggingProgressBridge::new("COM13");
    bridge.enter_region(FlashRegion::Firmware);
    bridge.init(0x10000, 1000);

    // Simulate espflash calling update every 10 bytes. After 1000
    // calls we should have logged at roughly 0, 10, 20, ..., 100%
    // — i.e. last_pct_logged should have landed on a 10-multiple.
    for current in (10..=1000).step_by(10) {
        bridge.update(current);
    }
    assert_eq!(bridge.last_current, 1000);
    assert_eq!(bridge.last_pct_logged % 10, 0);
    // Finishing resets the per-region state for the next region.
    bridge.finish(false);
}

#[test]
fn progress_bridge_handles_zero_total_without_panic() {
    use espflash::target::ProgressCallbacks;
    // A zero-byte region is defensive: espflash shouldn't ever
    // emit one, but our arithmetic must not divide by zero.
    let mut bridge = LoggingProgressBridge::new("COM13");
    bridge.enter_region(FlashRegion::Firmware);
    bridge.init(0x10000, 0);
    bridge.update(0);
    bridge.finish(true);
}

#[test]
fn progress_bridge_reports_correct_region_label() {
    let mut bridge = LoggingProgressBridge::new("COM13");
    assert_eq!(bridge.region_label(), "unknown");
    bridge.enter_region(FlashRegion::Bootloader);
    assert_eq!(bridge.region_label(), "bootloader");
    bridge.enter_region(FlashRegion::Partitions);
    assert_eq!(bridge.region_label(), "partitions");
    bridge.enter_region(FlashRegion::Firmware);
    assert_eq!(bridge.region_label(), "firmware");
}

#[test]
fn region_name_is_stable() {
    // Daemon log messages and integration tests depend on these
    // literal names. Pin them here so a refactor that renames
    // them trips a unit test first.
    assert_eq!(region_name(FlashRegion::Bootloader), "bootloader");
    assert_eq!(region_name(FlashRegion::Partitions), "partitions");
    assert_eq!(region_name(FlashRegion::Firmware), "firmware");
}
