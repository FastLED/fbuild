//! `uploadfs` single-region argv tests (FastLED/fbuild#1358).

use std::path::Path;

use super::deployer::Esp32Deployer;
use super::tests::test_esptool_params;

/// FastLED/fbuild#1358: `uploadfs` flashes one file to the offset the project's
/// partition CSV assigns the filesystem, so the argv must carry that offset
/// with no bootloader/partitions/firmware inference.
#[test]
fn build_flash_region_args_targets_the_caller_offset() {
    let params = test_esptool_params();
    let deployer = Esp32Deployer::new(
        "esp32s3", "921600", "0x0", "0x8000", "0x10000", &params, false,
    );
    let args = deployer.build_flash_region_args(
        Path::new("/proj/.pio/build/esp32s3/littlefs.bin"),
        "/dev/ttyACM0",
        "0x290000",
    );
    assert!(args.contains(&"write-flash".to_string()));
    assert!(args.contains(&"--flash-mode".to_string()));
    let offset_at = args
        .iter()
        .position(|a| a == "0x290000")
        .expect("filesystem partition offset");
    assert_eq!(
        args.get(offset_at + 1).map(String::as_str),
        Some("/proj/.pio/build/esp32s3/littlefs.bin")
    );
    // No inferred regions: the bootloader offset must never appear.
    assert!(!args.iter().any(|a| a == "0x0"));
    assert!(!args.iter().any(|a| a == "0x10000"));
    assert_eq!(
        args.last().map(String::as_str),
        Some("/proj/.pio/build/esp32s3/littlefs.bin")
    );
}

/// The verify half of `uploadfs --verify` uses the same offset/file pair, so a
/// mismatch can only come from the device, never from a different region.
#[test]
fn build_verify_region_args_mirrors_the_flash_offset() {
    let params = test_esptool_params();
    let deployer = Esp32Deployer::new(
        "esp32s3", "921600", "0x0", "0x8000", "0x10000", &params, false,
    );
    let args = deployer.build_verify_region_args(
        Path::new("/proj/.pio/build/esp32s3/spiffs.bin"),
        "/dev/ttyACM0",
        "0x290000",
    );
    assert!(args.contains(&"verify-flash".to_string()));
    let offset_at = args
        .iter()
        .position(|a| a == "0x290000")
        .expect("filesystem partition offset");
    assert_eq!(
        args.get(offset_at + 1).map(String::as_str),
        Some("/proj/.pio/build/esp32s3/spiffs.bin")
    );
}
