use std::path::Path;

use super::fs_utils::{collect_archive_files, find_framework_root};
use super::parsing::{parse_include_flags, split_defines};
use super::Esp32Framework;
use crate::{CacheSubdir, Package, PackageBase};

#[test]
fn test_esp32_framework_not_installed() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fw = Esp32Framework::with_cache_root(tmp.path(), &tmp.path().join("cache"), "esp32c6");
    assert!(!fw.is_installed());
}

#[test]
fn test_find_framework_root_direct() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(tmp.path().join("cores")).unwrap();
    assert_eq!(find_framework_root(tmp.path()), tmp.path().to_path_buf());
}

#[test]
fn test_find_framework_root_nested() {
    let tmp = tempfile::TempDir::new().unwrap();
    let nested = tmp.path().join("framework-arduinoespressif32");
    std::fs::create_dir_all(nested.join("cores")).unwrap();
    assert_eq!(find_framework_root(tmp.path()), nested);
}

#[test]
fn test_get_core_dir() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fw = Esp32Framework::new(tmp.path(), "esp32c6");
    let core_dir = fw.get_core_dir("esp32");
    assert!(core_dir.to_string_lossy().contains("cores"));
    assert!(core_dir.to_string_lossy().contains("esp32"));
}

#[test]
fn test_get_variant_dir() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fw = Esp32Framework::new(tmp.path(), "esp32c6");
    let variant_dir = fw.get_variant_dir("esp32c6");
    assert!(variant_dir.to_string_lossy().contains("variants"));
    assert!(variant_dir.to_string_lossy().contains("esp32c6"));
}

#[test]
fn test_sdk_paths() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fw = Esp32Framework::new(tmp.path(), "esp32c6");
    let ld_dir = fw.get_linker_scripts_dir("esp32c6");
    assert!(ld_dir.to_string_lossy().contains("sdk"));
    assert!(ld_dir.to_string_lossy().contains("esp32c6"));
    assert!(ld_dir.to_string_lossy().contains("ld"));
}

#[test]
fn test_collect_archive_files() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(tmp.path().join("libfreertos.a"), "").unwrap();
    std::fs::write(tmp.path().join("libesp_system.a"), "").unwrap();
    std::fs::write(tmp.path().join("readme.txt"), "").unwrap();
    let libs = collect_archive_files(tmp.path());
    assert_eq!(libs.len(), 2);
    assert!(libs.iter().all(|p| p.extension().unwrap() == "a"));
}

#[test]
fn test_get_sdk_libs_empty() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fw = Esp32Framework::new(tmp.path(), "esp32c6");
    let libs = fw.get_sdk_libs("esp32c6");
    assert!(libs.is_empty()); // No SDK installed
}

#[test]
fn test_validate_missing_cores() {
    let tmp = tempfile::TempDir::new().unwrap();
    let result = Esp32Framework::validate(tmp.path());
    assert!(result.is_err());
}

#[test]
fn test_validate_missing_arduino_h() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(tmp.path().join("cores").join("esp32")).unwrap();
    let result = Esp32Framework::validate(tmp.path());
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Arduino.h"));
}

#[test]
fn test_bootloader_bin_path() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fw = Esp32Framework::new(tmp.path(), "esp32c6");
    let boot = fw.get_bootloader_bin("esp32c6");
    assert!(boot.to_string_lossy().contains("bootloader.bin"));
}

#[test]
fn test_partitions_bin_path() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fw = Esp32Framework::new(tmp.path(), "esp32c6");
    let parts = fw.get_partitions_bin("esp32c6");
    assert!(parts.to_string_lossy().contains("partitions.bin"));
}

#[test]
fn test_boot_app0_bin_path() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fw = Esp32Framework::new(tmp.path(), "esp32c6");
    let boot_app0 = fw.get_boot_app0_bin();
    assert!(boot_app0.to_string_lossy().contains("boot_app0.bin"));
}

#[test]
fn test_parse_iwithprefixbefore_format() {
    let tmp = tempfile::TempDir::new().unwrap();
    let include_base = tmp.path().join("include");

    // Create dirs that match the relative paths
    let freertos = include_base.join("freertos/include/freertos");
    let esp_sys = include_base.join("esp_system/include");
    std::fs::create_dir_all(&freertos).unwrap();
    std::fs::create_dir_all(&esp_sys).unwrap();

    // This is the actual format from flags/includes files
    let content =
        "-iwithprefixbefore freertos/include/freertos -iwithprefixbefore esp_system/include";
    let dirs = parse_include_flags(content, &include_base, tmp.path());

    assert_eq!(dirs.len(), 2);
    assert_eq!(dirs[0], freertos);
    assert_eq!(dirs[1], esp_sys);
}

#[test]
fn test_sdk_include_dirs_with_mock() {
    let tmp = tempfile::TempDir::new().unwrap();
    // Create mock SDK structure with includes file
    let sdk_dir = tmp.path().join("tools").join("sdk").join("esp32c6");
    let flags_dir = sdk_dir.join("flags");
    std::fs::create_dir_all(&flags_dir).unwrap();

    // Create some include dirs
    let inc1 = sdk_dir.join("include").join("freertos");
    let inc2 = sdk_dir.join("include").join("esp_system");
    std::fs::create_dir_all(&inc1).unwrap();
    std::fs::create_dir_all(&inc2).unwrap();

    // Write includes file with absolute paths
    let includes_content = format!("-I{}\n-I{}\n", inc1.display(), inc2.display());
    std::fs::write(flags_dir.join("includes"), &includes_content).unwrap();

    let fw = Esp32Framework {
        base: PackageBase::new(
            "test",
            "1.0",
            "http://example.com",
            "http://example.com",
            None,
            CacheSubdir::Platforms,
            tmp.path(),
        ),
        install_dir: Some(tmp.path().to_path_buf()),
    };

    let dirs = fw.get_sdk_include_dirs("esp32c6", None);
    assert_eq!(dirs.len(), 2);
}

#[test]
fn test_sdk_include_dirs_prefers_requested_memory_variant() {
    let tmp = tempfile::TempDir::new().unwrap();
    let sdk_dir = tmp.path().join("tools").join("sdk").join("esp32s3");
    let flags_dir = sdk_dir.join("flags");
    std::fs::create_dir_all(&flags_dir).unwrap();
    std::fs::create_dir_all(sdk_dir.join("include")).unwrap();
    std::fs::write(flags_dir.join("includes"), "").unwrap();
    std::fs::create_dir_all(sdk_dir.join("qio_opi").join("include")).unwrap();
    std::fs::create_dir_all(sdk_dir.join("dio_qspi").join("include")).unwrap();

    let fw = Esp32Framework {
        base: PackageBase::new(
            "test",
            "1.0",
            "http://example.com",
            "http://example.com",
            None,
            CacheSubdir::Platforms,
            tmp.path(),
        ),
        install_dir: Some(tmp.path().to_path_buf()),
    };

    let dirs = fw.get_sdk_include_dirs("esp32s3", Some("dio_qspi"));
    assert!(dirs
        .iter()
        .any(|d| d.ends_with(Path::new("dio_qspi").join("include"))));
    assert!(!dirs
        .iter()
        .any(|d| d.ends_with(Path::new("qio_opi").join("include"))));
}

#[test]
fn test_sdk_lib_flags_prefers_requested_memory_variant() {
    let tmp = tempfile::TempDir::new().unwrap();
    let sdk_dir = tmp.path().join("tools").join("sdk").join("esp32s3");
    let flags_dir = sdk_dir.join("flags");
    std::fs::create_dir_all(&flags_dir).unwrap();
    std::fs::write(flags_dir.join("ld_libs"), "-lfoo").unwrap();
    std::fs::create_dir_all(sdk_dir.join("lib")).unwrap();
    std::fs::create_dir_all(sdk_dir.join("dio_qspi")).unwrap();
    std::fs::create_dir_all(sdk_dir.join("qio_opi")).unwrap();

    let fw = Esp32Framework {
        base: PackageBase::new(
            "test",
            "1.0",
            "http://example.com",
            "http://example.com",
            None,
            CacheSubdir::Platforms,
            tmp.path(),
        ),
        install_dir: Some(tmp.path().to_path_buf()),
    };

    let flags = fw.get_sdk_lib_flags("esp32s3", Some("dio_qspi"));
    assert!(flags
        .iter()
        .any(|f| f.ends_with("\\esp32s3\\dio_qspi") || f.ends_with("/esp32s3/dio_qspi")));
    assert!(!flags
        .iter()
        .any(|f| f.ends_with("\\esp32s3\\qio_opi") || f.ends_with("/esp32s3/qio_opi")));
}

#[test]
fn test_split_defines_preserves_escaped_quotes() {
    let content =
        r#"-DFOO=1 -DMBEDTLS_CONFIG_FILE=\"mbedtls/esp_config.h\" -DBAR -DIDF_VER=\"v5.5.2\""#;
    let tokens = split_defines(content);
    assert_eq!(tokens.len(), 4);
    assert_eq!(tokens[0], "-DFOO=1");
    assert_eq!(
        tokens[1],
        r#"-DMBEDTLS_CONFIG_FILE=\"mbedtls/esp_config.h\""#
    );
    assert_eq!(tokens[2], "-DBAR");
    assert_eq!(tokens[3], r#"-DIDF_VER=\"v5.5.2\""#);
}

#[test]
fn test_split_defines_empty() {
    assert!(split_defines("").is_empty());
    assert!(split_defines("   ").is_empty());
}

#[test]
fn test_split_defines_single() {
    assert_eq!(split_defines("-DFOO=1"), vec!["-DFOO=1"]);
}
