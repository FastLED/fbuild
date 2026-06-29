//! Unit and hardware-gated integration tests for the esp32 module.

#![cfg(test)]

use std::path::Path;

use sha2::{Digest, Sha256};

use super::deployer::{Esp32Deployer, EsptoolParams};
use super::image::patch_bytes;
use super::image::{
    repair_esp_image_checksum_and_hash, resolve_esp_image_file_offset, ESP_IMAGE_APPENDED_HASH_LEN,
    ESP_IMAGE_HEADER_LEN, ESP_IMAGE_HEADER_MAGIC, ESP_IMAGE_SEGMENT_HEADER_LEN,
    ESP_ROM_CHECKSUM_INITIAL, QEMU_ADC_CALIBRATION_EXPECTED_BYTES,
    QEMU_ADC_CALIBRATION_PATCH_BYTES,
};
use super::qemu::{
    build_qemu_args, build_qemu_esp32s3_args, create_qemu_flash_image,
    resolve_qemu_flash_size_bytes,
};
use super::verify::{parse_verify_regions, FlashRegion, RegionVerifyResult, VerifyOutcome};
use crate::Deployer;

/// Test params matching ESP32-C6 JSON config values.
fn test_esptool_params() -> EsptoolParams {
    EsptoolParams {
        flash_mode: "dio".to_string(),
        flash_freq: "80m".to_string(),
        flash_size: "4MB".to_string(),
        default_baud: "460800".to_string(),
        before_reset: "default-reset".to_string(),
        after_reset: "hard-reset".to_string(),
    }
}

#[test]
fn test_esp32_deployer_creation() {
    let params = test_esptool_params();
    let deployer = Esp32Deployer::new(
        "esp32c6", "460800", "0x0", "0x8000", "0x10000", &params, false,
    );
    assert_eq!(deployer.chip, "esp32c6");
    assert_eq!(deployer.baud_rate, "460800");
    assert_eq!(deployer.bootloader_offset, "0x0");
    assert_eq!(deployer.firmware_offset, "0x10000");
    assert_eq!(deployer.flash_mode, "dio");
    assert_eq!(deployer.flash_size, "4MB");
    assert_eq!(deployer.before_reset, "default-reset");
}

#[test]
fn qemu_flash_size_resolution_accepts_supported_sizes() {
    let mut board = fbuild_test_support::board_for_test("esp32-s3-devkitc-1");
    board.max_flash = Some(8 * 1024 * 1024);
    assert_eq!(
        resolve_qemu_flash_size_bytes(&board, "4MB").unwrap(),
        8 * 1024 * 1024
    );
}

#[test]
fn qemu_flash_size_resolution_rejects_unsupported_size() {
    let mut board = fbuild_test_support::board_for_test("esp32-s3-devkitc-1");
    board.max_flash = Some(32 * 1024 * 1024);
    let err = resolve_qemu_flash_size_bytes(&board, "4MB").unwrap_err();
    assert!(err
        .to_string()
        .contains("supports only 2MB, 4MB, 8MB, or 16MB"));
}

#[test]
fn create_qemu_flash_image_writes_regions_at_offsets() {
    let tmp = tempfile::TempDir::new().unwrap();
    let build_dir = tmp.path().join("build");
    std::fs::create_dir_all(&build_dir).unwrap();

    let boot = build_dir.join("bootloader.bin");
    let parts = build_dir.join("partitions.bin");
    let fw = build_dir.join("firmware.bin");
    std::fs::write(&boot, b"BOOT").unwrap();
    std::fs::write(&parts, b"PART").unwrap();
    std::fs::write(&fw, b"FIRM").unwrap();

    let flash = tmp.path().join("qemu_flash.bin");
    create_qemu_flash_image(
        &fw,
        &flash,
        2 * 1024 * 1024,
        "0x0",
        "0x8000",
        "0x10000",
        None,
    )
    .unwrap();

    let bytes = std::fs::read(&flash).unwrap();
    assert_eq!(&bytes[0..4], b"BOOT");
    assert_eq!(&bytes[0x8000..0x8004], b"PART");
    assert_eq!(&bytes[0x10000..0x10004], b"FIRM");
    assert_eq!(bytes.len(), 2 * 1024 * 1024);
    assert_eq!(bytes[0x200], 0xFF);
}

#[test]
fn create_qemu_flash_image_includes_boot_app0_when_present() {
    let tmp = tempfile::TempDir::new().unwrap();
    let build_dir = tmp.path().join("build");
    std::fs::create_dir_all(&build_dir).unwrap();

    let boot = build_dir.join("bootloader.bin");
    let boot_app0 = build_dir.join("boot_app0.bin");
    let parts = build_dir.join("partitions.bin");
    let fw = build_dir.join("firmware.bin");
    std::fs::write(&boot, b"BOOT").unwrap();
    std::fs::write(&boot_app0, b"APP0").unwrap();
    std::fs::write(&parts, b"PART").unwrap();
    std::fs::write(&fw, b"FIRM").unwrap();

    let flash = tmp.path().join("qemu_flash.bin");
    create_qemu_flash_image(
        &fw,
        &flash,
        2 * 1024 * 1024,
        "0x0",
        "0x8000",
        "0x10000",
        None,
    )
    .unwrap();

    let bytes = std::fs::read(&flash).unwrap();
    assert_eq!(&bytes[0xE000..0xE004], b"APP0");
}

#[test]
fn resolve_esp_image_file_offset_maps_address_into_segment_data() {
    let mut image = vec![0u8; ESP_IMAGE_HEADER_LEN];
    image[0] = ESP_IMAGE_HEADER_MAGIC;
    image[1] = 1;
    image.extend_from_slice(&0x4200_0000u32.to_le_bytes());
    image.extend_from_slice(&6u32.to_le_bytes());
    image.extend_from_slice(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);

    let offset = resolve_esp_image_file_offset(&image, 0x4200_0003).unwrap();
    assert_eq!(
        offset,
        ESP_IMAGE_HEADER_LEN + ESP_IMAGE_SEGMENT_HEADER_LEN + 3
    );
}

#[test]
fn patch_bytes_rewrites_expected_bytes_only() {
    let mut flash = [0xFFu8; 16];
    flash[6..8].copy_from_slice(&QEMU_ADC_CALIBRATION_EXPECTED_BYTES);

    patch_bytes(
        &mut flash,
        6,
        &QEMU_ADC_CALIBRATION_EXPECTED_BYTES,
        &QEMU_ADC_CALIBRATION_PATCH_BYTES,
    )
    .unwrap();

    assert_eq!(&flash[6..8], &QEMU_ADC_CALIBRATION_PATCH_BYTES);
}

#[test]
fn repair_esp_image_checksum_and_hash_updates_trailers_after_patch() {
    let mut image = vec![0u8; ESP_IMAGE_HEADER_LEN];
    image[0] = ESP_IMAGE_HEADER_MAGIC;
    image[1] = 1;
    image[23] = 1;
    image.extend_from_slice(&0x4200_0000u32.to_le_bytes());
    image.extend_from_slice(&8u32.to_le_bytes());
    image.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
    image.extend_from_slice(&[0u8; 16]);
    image.extend_from_slice(&[0u8; ESP_IMAGE_APPENDED_HASH_LEN]);

    patch_bytes(
        &mut image,
        ESP_IMAGE_HEADER_LEN + ESP_IMAGE_SEGMENT_HEADER_LEN + 3,
        &[4],
        &[9],
    )
    .unwrap();
    repair_esp_image_checksum_and_hash(&mut image).unwrap();

    let checksum_offset =
        ((ESP_IMAGE_HEADER_LEN + ESP_IMAGE_SEGMENT_HEADER_LEN + 8 + 1 + 15) & !15) - 1;
    let expected_checksum = {
        let mut checksum_word = ESP_ROM_CHECKSUM_INITIAL;
        for chunk in image[ESP_IMAGE_HEADER_LEN + ESP_IMAGE_SEGMENT_HEADER_LEN
            ..ESP_IMAGE_HEADER_LEN + ESP_IMAGE_SEGMENT_HEADER_LEN + 8]
            .chunks(4)
        {
            let mut word = [0u8; 4];
            word[..chunk.len()].copy_from_slice(chunk);
            checksum_word ^= u32::from_le_bytes(word);
        }
        ((checksum_word >> 24) ^ (checksum_word >> 16) ^ (checksum_word >> 8) ^ checksum_word) as u8
    };
    let expected_hash = Sha256::digest(&image[..checksum_offset + 1]);
    assert_eq!(image[checksum_offset], expected_checksum);
    assert_eq!(
        &image[checksum_offset + 1..checksum_offset + 1 + ESP_IMAGE_APPENDED_HASH_LEN],
        expected_hash.as_slice()
    );
}

#[test]
fn qemu_command_builder_uses_expected_machine_and_watchdog_override() {
    let args = build_qemu_esp32s3_args(Path::new("flash.bin"), None);
    assert!(args.contains(&"esp32s3".to_string()));
    assert!(args
        .iter()
        .any(|arg| arg == "driver=timer.esp32s3.timg,property=wdt_disable,value=true"));
    assert!(args
        .iter()
        .any(|arg| arg.contains("file=flash.bin,if=mtd,format=raw")));
}

#[test]
fn qemu_command_builder_uses_esp32_machine_for_base_variant() {
    let args = build_qemu_args("esp32", Path::new("flash.bin"), None);
    assert!(args.contains(&"esp32".to_string()));
    assert!(args
        .iter()
        .any(|arg| arg == "driver=timer.esp32.timg,property=wdt_disable,value=true"));
    assert!(args
        .iter()
        .any(|arg| arg.contains("file=flash.bin,if=mtd,format=raw")));
}

#[test]
fn qemu_command_builder_adds_psram_args_when_requested() {
    let args = build_qemu_esp32s3_args(
        Path::new("flash.bin"),
        Some(fbuild_config::Esp32QemuPsramConfig {
            size_mib: 8,
            is_octal: true,
        }),
    );
    assert!(args.windows(2).any(|pair| pair == ["-m", "8M"]));
    assert!(args
        .iter()
        .any(|arg| arg == "driver=ssi_psram,property=is_octal,value=true"));
}

#[test]
fn test_esp32_deployer_from_board_config() {
    let board = fbuild_test_support::board_for_test("esp32c6");
    let params = test_esptool_params();
    let deployer =
        Esp32Deployer::from_board_config(&board, "0x0", "0x8000", "0x10000", &params, false);
    assert_eq!(deployer.chip, "esp32c6");
    assert_eq!(deployer.bootloader_offset, "0x0");
}

#[test]
fn test_esp32_deployer_from_board_config_honors_flash_size_override() {
    let mut overrides = std::collections::HashMap::new();
    overrides.insert("flash_size".to_string(), "4MB".to_string());
    let board =
        fbuild_test_support::board_for_test_with_overrides("esp32-c6-devkitc-1", &overrides);
    let params = test_esptool_params();

    let deployer =
        Esp32Deployer::from_board_config(&board, "0x0", "0x8000", "0x10000", &params, false);

    assert_eq!(deployer.flash_size, "4MB");
}

#[test]
#[cfg(feature = "espflash-native")]
fn native_write_is_disabled_for_known_stalling_chips() {
    let params = test_esptool_params();
    let c6 = Esp32Deployer::new(
        "esp32c6", "460800", "0x0", "0x8000", "0x10000", &params, false,
    )
    .with_native_write(true);
    let s3 = Esp32Deployer::new(
        "esp32s3", "460800", "0x0", "0x8000", "0x10000", &params, false,
    )
    .with_native_write(true);
    let c3 = Esp32Deployer::new(
        "esp32c3", "460800", "0x0", "0x8000", "0x10000", &params, false,
    )
    .with_native_write(true);

    assert!(!c6.use_native_write);
    assert!(!s3.use_native_write);
    assert!(c3.use_native_write);
}

#[test]
fn test_deploy_requires_port() {
    let params = test_esptool_params();
    let deployer = Esp32Deployer::new(
        "esp32c6", "460800", "0x0", "0x8000", "0x10000", &params, false,
    );
    let tmp = tempfile::TempDir::new().unwrap();
    let result = deployer.deploy(tmp.path(), "esp32c6", Path::new("firmware.bin"), None);
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("serial port required"));
}

/// Fast deploy: the verify-flash command line must include the
/// `verify-flash` subcommand and pair every flash region with its
/// matching offset, in the order bootloader, partitions, firmware.
/// Verifying all three in a single esptool call amortises the
/// ~3-second stub flasher upload.
#[test]
fn build_verify_flash_args_includes_all_three_regions_when_present() {
    let params = test_esptool_params();
    let deployer = Esp32Deployer::new(
        "esp32s3", "921600", "0x0", "0x8000", "0x10000", &params, false,
    );
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(tmp.path().join("bootloader.bin"), b"boot").unwrap();
    std::fs::write(tmp.path().join("partitions.bin"), b"part").unwrap();
    let fw = tmp.path().join("firmware.bin");
    std::fs::write(&fw, b"firm").unwrap();

    let args = deployer.build_verify_flash_args(&fw, "COM13");

    // Subcommand
    assert!(
        args.contains(&"verify-flash".to_string()),
        "missing verify-flash subcommand: {:?}",
        args
    );
    // Chip + port
    assert!(args.contains(&"--chip".to_string()));
    assert!(args.contains(&"esp32s3".to_string()));
    assert!(args.contains(&"--port".to_string()));
    assert!(args.contains(&"COM13".to_string()));

    // All three (offset, file) pairs in the right order.
    let pos_verify = args.iter().position(|a| a == "verify-flash").unwrap();
    let pos_boot = args.iter().position(|a| a == "0x0").unwrap();
    let pos_parts = args.iter().position(|a| a == "0x8000").unwrap();
    let pos_fw = args.iter().position(|a| a == "0x10000").unwrap();
    assert!(
        pos_verify < pos_boot && pos_boot < pos_parts && pos_parts < pos_fw,
        "regions must appear after verify-flash in bootloader→partitions→firmware order: {:?}",
        args
    );

    // verify-flash MUST NOT carry --flash-mode/freq/size flags;
    // those are write-flash options and esptool 5.x rejects them
    // here. We were burned by this when we copied the deploy()
    // argument layout wholesale.
    assert!(
        !args.contains(&"--flash-mode".to_string()),
        "verify-flash must not include --flash-mode (write-flash only): {:?}",
        args
    );
    assert!(
        !args.contains(&"--flash-freq".to_string()),
        "verify-flash must not include --flash-freq (write-flash only): {:?}",
        args
    );
}

#[test]
fn build_verify_flash_args_skips_missing_bootloader_and_partitions() {
    // When bootloader.bin / partitions.bin haven't been built (e.g.
    // an upload-only test fixture), verify must still cover firmware
    // alone. Otherwise we'd skip the only thing we have.
    let params = test_esptool_params();
    let deployer = Esp32Deployer::new(
        "esp32s3", "921600", "0x0", "0x8000", "0x10000", &params, false,
    );
    let tmp = tempfile::TempDir::new().unwrap();
    let fw = tmp.path().join("firmware.bin");
    std::fs::write(&fw, b"firm").unwrap();

    let args = deployer.build_verify_flash_args(&fw, "COM13");

    // No bootloader or partitions paths in the args.
    assert!(!args.iter().any(|a| a.ends_with("bootloader.bin")));
    assert!(!args.iter().any(|a| a.ends_with("partitions.bin")));
    // Firmware offset is still present.
    assert!(args.contains(&"0x10000".to_string()));
    assert!(args.iter().any(|a| a.ends_with("firmware.bin")));
}

/// esptool 5.x emits one `Verifying ... at 0x{addr:#010x} ...` line
/// followed by `Verification successful/failed` for each region.
/// Parser must pair them and classify the region by offset.
#[test]
fn parse_verify_regions_classifies_each_region_by_offset() {
    let stdout = "\
Verifying 0x6060 (24672) bytes at 0x00000000 in flash against 'bootloader.bin'...\n\
Verification successful (digest matched).\n\
Verifying 0xc00 (3072) bytes at 0x00008000 in flash against 'partitions.bin'...\n\
Verification successful (digest matched).\n\
Verifying 0x260a60 (2493536) bytes at 0x00010000 in flash against 'firmware.bin'...\n\
Verification failed (digest mismatch).\n";
    let regions = parse_verify_regions(stdout, "0x0", "0x8000", "0x10000");
    assert_eq!(
        regions,
        vec![
            RegionVerifyResult {
                region: FlashRegion::Bootloader,
                matched: true
            },
            RegionVerifyResult {
                region: FlashRegion::Partitions,
                matched: true
            },
            RegionVerifyResult {
                region: FlashRegion::Firmware,
                matched: false
            },
        ]
    );
}

/// When esptool output doesn't match the expected pattern (older
/// version, localized output, truncated log), parse returns an empty
/// vec so the daemon falls back to flashing all regions.
#[test]
fn parse_verify_regions_returns_empty_on_unknown_format() {
    let stdout = "some unrelated failure output\nexit 1\n";
    let regions = parse_verify_regions(stdout, "0x0", "0x8000", "0x10000");
    assert!(regions.is_empty());
}

/// Regions whose offset doesn't match any of the three knowns are
/// silently skipped — we stay conservative and return only what we
/// understand.
#[test]
fn parse_verify_regions_skips_unknown_offsets() {
    let stdout = "\
Verifying 0x1000 (4096) bytes at 0x00001000 in flash against 'bootloader.bin'...\n\
Verification failed (digest mismatch).\n\
Verifying 0x1000 (4096) bytes at 0x00010000 in flash against 'firmware.bin'...\n\
Verification successful (digest matched).\n";
    let regions = parse_verify_regions(stdout, "0x1000", "0x8000", "0x10000");
    assert_eq!(
        regions,
        vec![
            RegionVerifyResult {
                region: FlashRegion::Bootloader,
                matched: false
            },
            RegionVerifyResult {
                region: FlashRegion::Firmware,
                matched: true
            },
        ]
    );
}

/// The selective write-flash argv must include the write-flash
/// subcommand and only the requested region's offset/file pair.
/// Skipping bootloader + partitions is the ~1s save targeted by #67.
#[test]
fn build_write_flash_args_firmware_only_skips_bootloader_and_partitions() {
    let params = test_esptool_params();
    let deployer = Esp32Deployer::new(
        "esp32s3", "921600", "0x0", "0x8000", "0x10000", &params, false,
    );
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(tmp.path().join("bootloader.bin"), b"boot").unwrap();
    std::fs::write(tmp.path().join("partitions.bin"), b"part").unwrap();
    let fw = tmp.path().join("firmware.bin");
    std::fs::write(&fw, b"firm").unwrap();

    let args = deployer.build_write_flash_args(&fw, "COM13", Some(&[FlashRegion::Firmware]));

    assert!(args.contains(&"write-flash".to_string()));
    assert!(args.windows(2).any(|pair| pair == ["--flash-size", "4MB"]));
    assert!(!args.contains(&"detect".to_string()));
    assert!(!args.iter().any(|a| a.ends_with("bootloader.bin")));
    assert!(!args.iter().any(|a| a.ends_with("partitions.bin")));
    assert!(args.contains(&"0x10000".to_string()));
    assert!(args.iter().any(|a| a.ends_with("firmware.bin")));
    assert!(!args.contains(&"0x8000".to_string()));
}

/// `None` regions (default deploy) must still include all three
/// present files — we can't regress the baseline path.
#[test]
fn build_write_flash_args_default_includes_all_regions() {
    let params = test_esptool_params();
    let deployer = Esp32Deployer::new(
        "esp32s3", "921600", "0x0", "0x8000", "0x10000", &params, false,
    );
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(tmp.path().join("bootloader.bin"), b"boot").unwrap();
    std::fs::write(tmp.path().join("partitions.bin"), b"part").unwrap();
    let fw = tmp.path().join("firmware.bin");
    std::fs::write(&fw, b"firm").unwrap();

    let args = deployer.build_write_flash_args(&fw, "COM13", None);
    assert!(args.contains(&"0x0".to_string()));
    assert!(args.contains(&"0x8000".to_string()));
    assert!(args.contains(&"0x10000".to_string()));
    assert!(args.windows(2).any(|pair| pair == ["--flash-size", "4MB"]));
}

/// If a caller requests a region whose file is missing on disk, fail
/// with a clear error rather than silently emitting a write-flash
/// call with no offset/file pair (which would produce an opaque
/// esptool usage error). Addresses CodeRabbit review on PR #71.
#[tokio::test]
async fn deploy_regions_errors_when_requested_region_file_missing() {
    let params = test_esptool_params();
    let deployer = Esp32Deployer::new(
        "esp32s3", "921600", "0x0", "0x8000", "0x10000", &params, false,
    );
    let tmp = tempfile::TempDir::new().unwrap();
    let fw = tmp.path().join("firmware.bin");
    std::fs::write(&fw, b"firm").unwrap();
    // Note: no bootloader.bin written.
    let err = deployer
        .deploy_regions(&fw, "COM13", &[FlashRegion::Bootloader])
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("bootloader.bin"),
        "error must name the missing file: {}",
        err
    );
}

/// Empty region slice -> usage error; we surface it rather than let
/// esptool barf.
#[tokio::test]
async fn deploy_regions_rejects_empty_slice() {
    let params = test_esptool_params();
    let deployer = Esp32Deployer::new(
        "esp32s3", "921600", "0x0", "0x8000", "0x10000", &params, false,
    );
    let err = deployer
        .deploy_regions(Path::new("firmware.bin"), "COM13", &[])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no regions"));
}

#[test]
fn verify_outcome_is_match_helper() {
    let m = VerifyOutcome::Match {
        stdout: "ok".into(),
        stderr: String::new(),
    };
    let mm = VerifyOutcome::Mismatch {
        stdout: String::new(),
        stderr: "Verification failed".into(),
        regions: Vec::new(),
    };
    assert!(m.is_match());
    assert!(!mm.is_match());
}

// ---------------------------------------------------------------
// Hardware-gated verify-deployment tests for each ESP32 family MCU.
//
// These tests are `#[ignore]` so they never run in CI.  To exercise
// them on a local bench, set the env vars described below and run:
//
//   soldr cargo test -p fbuild-deploy esp32::tests::try_verify_deployment_real -- --ignored --nocapture
//
// Each test reads **two** environment variables:
//
//   <MCU>_PORT        – serial port the board is attached to (e.g. COM13, /dev/ttyUSB0)
//   <MCU>_FIRMWARE    – absolute path to a pre-flashed firmware.bin
//
// where <MCU> is one of ESP32, ESP32S2, ESP32S3, ESP32C2, ESP32C3,
// ESP32C6, ESP32H2, ESP32P4.
//
// The firmware directory must also contain `bootloader.bin` and
// `partitions.bin` so that verify-flash can check all three regions
// in a single esptool invocation.
//
// Bootloader offsets per chip (from esp32.rs header comment):
//   0x1000 – esp32, esp32s2
//   0x0    – esp32c2, esp32c3, esp32c5, esp32c6, esp32h2, esp32s3
//   0x2000 – esp32p4
// ---------------------------------------------------------------

/// Shared implementation for all per-chip hardware-gated verify tests.
///
/// 1. Reads `{port_env}` and `{firmware_env}` from the environment.
/// 2. Asserts that verify against the pre-flashed image returns `Match`
///    in under 15 seconds.
/// 3. Asserts that a tampered image (1 byte flipped) returns `Mismatch`.
async fn run_verify_deployment_test(
    chip: &str,
    bootloader_offset: &str,
    port_env: &str,
    firmware_env: &str,
) {
    let port = std::env::var(port_env).unwrap_or_else(|_| {
        panic!(
            "set {} to the serial port your {} board is attached to (e.g. COM13)",
            port_env, chip
        )
    });
    let firmware_path = std::env::var(firmware_env).unwrap_or_else(|_| {
        panic!(
            "set {} to the absolute path of the pre-flashed firmware.bin for {}",
            firmware_env, chip
        )
    });
    let reference = std::path::PathBuf::from(&firmware_path);
    assert!(
        reference.is_file(),
        "reference firmware not found at {}; build and flash it first",
        reference.display()
    );
    let ref_dir = reference
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    for name in ["bootloader.bin", "partitions.bin"] {
        let artifact = ref_dir.join(name);
        assert!(
            artifact.is_file(),
            "[{}] missing {} next to {}; otherwise this only verifies firmware.bin",
            chip,
            name,
            reference.display()
        );
    }

    let params = EsptoolParams {
        flash_mode: "dio".to_string(),
        flash_freq: "80m".to_string(),
        flash_size: "4MB".to_string(),
        default_baud: "921600".to_string(),
        before_reset: "default-reset".to_string(),
        after_reset: "hard-reset".to_string(),
    };
    let deployer = Esp32Deployer::new(
        chip,
        "921600",
        bootloader_offset,
        "0x8000",
        "0x10000",
        &params,
        true,
    );

    // Phase 1: matching image -> Match
    let start = std::time::Instant::now();
    let outcome = deployer
        .try_verify_deployment(&reference, &port)
        .await
        .unwrap_or_else(|e| panic!("verify must not fail against attached {}: {}", chip, e));
    let elapsed = start.elapsed();
    assert!(
        outcome.is_match(),
        "[{}] expected Match against pre-flashed firmware; got {:?}",
        chip,
        outcome
    );
    assert!(
        elapsed < std::time::Duration::from_secs(15),
        "[{}] verify took {:?} -- should complete in <15s",
        chip,
        elapsed
    );
    eprintln!("[{}] verify (Match) elapsed: {:?}", chip, elapsed);

    // Phase 2: tampered image -> Mismatch
    let tmp = tempfile::TempDir::new().unwrap();
    // Copy bootloader and partitions next to the tampered firmware so
    // build_verify_flash_args picks them up alongside firmware.bin.
    for name in ["bootloader.bin", "partitions.bin"] {
        std::fs::copy(ref_dir.join(name), tmp.path().join(name)).unwrap();
    }
    let tampered = tmp.path().join("firmware.bin");
    let mut bytes = std::fs::read(&reference).unwrap();
    // Flip a byte well past the image header to avoid invalidating
    // the ESP-IDF magic and triggering an esptool parse error rather
    // than a clean digest mismatch.
    let target = bytes.len() / 2;
    bytes[target] ^= 0x55;
    std::fs::write(&tampered, &bytes).unwrap();

    let outcome = deployer
        .try_verify_deployment(&tampered, &port)
        .await
        .unwrap_or_else(|e| {
            panic!(
                "[{}] verify must not fail with tampered firmware: {}",
                chip, e
            )
        });
    assert!(
        !outcome.is_match(),
        "[{}] expected Mismatch for tampered firmware; got {:?}",
        chip,
        outcome
    );
    eprintln!("[{}] verify (Mismatch) detected correctly", chip);
}

/// ESP32 (Xtensa, bootloader at 0x1000).
///
/// ```text
/// ESP32_PORT=COM5 ESP32_FIRMWARE=C:\path\to\firmware.bin \
///   soldr cargo test -p fbuild-deploy esp32::tests::try_verify_deployment_real_esp32 -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore = "requires real ESP32 board — set ESP32_PORT and ESP32_FIRMWARE"]
async fn try_verify_deployment_real_esp32() {
    run_verify_deployment_test("esp32", "0x1000", "ESP32_PORT", "ESP32_FIRMWARE").await;
}

/// ESP32-S2 (Xtensa single-core, bootloader at 0x1000).
///
/// ```text
/// ESP32S2_PORT=COM6 ESP32S2_FIRMWARE=C:\path\to\firmware.bin \
///   soldr cargo test -p fbuild-deploy esp32::tests::try_verify_deployment_real_esp32s2 -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore = "requires real ESP32-S2 board — set ESP32S2_PORT and ESP32S2_FIRMWARE"]
async fn try_verify_deployment_real_esp32s2() {
    run_verify_deployment_test("esp32s2", "0x1000", "ESP32S2_PORT", "ESP32S2_FIRMWARE").await;
}

/// ESP32-S3 (Xtensa dual-core, bootloader at 0x0).
///
/// This is the original baseline test, now using env-var configuration
/// consistent with the rest of the family.
///
/// ```text
/// ESP32S3_PORT=COM13 ESP32S3_FIRMWARE=C:\Users\niteris\dev\fastled\.pio\build\esp32s3\firmware.bin \
///   soldr cargo test -p fbuild-deploy esp32::tests::try_verify_deployment_real_esp32s3 -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore = "requires real ESP32-S3 board — set ESP32S3_PORT and ESP32S3_FIRMWARE"]
async fn try_verify_deployment_real_esp32s3() {
    run_verify_deployment_test("esp32s3", "0x0", "ESP32S3_PORT", "ESP32S3_FIRMWARE").await;
}

/// ESP32-C2 (RISC-V single-core, bootloader at 0x0).
///
/// ```text
/// ESP32C2_PORT=COM7 ESP32C2_FIRMWARE=C:\path\to\firmware.bin \
///   soldr cargo test -p fbuild-deploy esp32::tests::try_verify_deployment_real_esp32c2 -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore = "requires real ESP32-C2 board — set ESP32C2_PORT and ESP32C2_FIRMWARE"]
async fn try_verify_deployment_real_esp32c2() {
    run_verify_deployment_test("esp32c2", "0x0", "ESP32C2_PORT", "ESP32C2_FIRMWARE").await;
}

/// ESP32-C3 (RISC-V single-core, bootloader at 0x0).
///
/// ```text
/// ESP32C3_PORT=COM8 ESP32C3_FIRMWARE=C:\path\to\firmware.bin \
///   soldr cargo test -p fbuild-deploy esp32::tests::try_verify_deployment_real_esp32c3 -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore = "requires real ESP32-C3 board — set ESP32C3_PORT and ESP32C3_FIRMWARE"]
async fn try_verify_deployment_real_esp32c3() {
    run_verify_deployment_test("esp32c3", "0x0", "ESP32C3_PORT", "ESP32C3_FIRMWARE").await;
}

/// ESP32-C6 (RISC-V single-core, bootloader at 0x0).
///
/// ```text
/// ESP32C6_PORT=COM9 ESP32C6_FIRMWARE=C:\path\to\firmware.bin \
///   soldr cargo test -p fbuild-deploy esp32::tests::try_verify_deployment_real_esp32c6 -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore = "requires real ESP32-C6 board — set ESP32C6_PORT and ESP32C6_FIRMWARE"]
async fn try_verify_deployment_real_esp32c6() {
    run_verify_deployment_test("esp32c6", "0x0", "ESP32C6_PORT", "ESP32C6_FIRMWARE").await;
}

/// ESP32-H2 (RISC-V single-core, bootloader at 0x0).
///
/// ```text
/// ESP32H2_PORT=COM10 ESP32H2_FIRMWARE=C:\path\to\firmware.bin \
///   soldr cargo test -p fbuild-deploy esp32::tests::try_verify_deployment_real_esp32h2 -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore = "requires real ESP32-H2 board — set ESP32H2_PORT and ESP32H2_FIRMWARE"]
async fn try_verify_deployment_real_esp32h2() {
    run_verify_deployment_test("esp32h2", "0x0", "ESP32H2_PORT", "ESP32H2_FIRMWARE").await;
}

/// ESP32-P4 (RISC-V dual-core, OPI flash, bootloader at 0x2000).
///
/// Note: ESP32-P4 uses OPI flash and has a different bootloader offset
/// (0x2000) compared to other ESP32 chips.
///
/// ```text
/// ESP32P4_PORT=COM11 ESP32P4_FIRMWARE=C:\path\to\firmware.bin \
///   soldr cargo test -p fbuild-deploy esp32::tests::try_verify_deployment_real_esp32p4 -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore = "requires real ESP32-P4 board — set ESP32P4_PORT and ESP32P4_FIRMWARE"]
async fn try_verify_deployment_real_esp32p4() {
    run_verify_deployment_test("esp32p4", "0x2000", "ESP32P4_PORT", "ESP32P4_FIRMWARE").await;
}
