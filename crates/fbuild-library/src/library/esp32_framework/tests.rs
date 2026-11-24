use std::path::Path;

use super::Esp32Framework;
use super::fs_utils::{collect_archive_files, find_framework_root};
use super::parsing::{parse_include_flags, parse_pio_cppdefines, split_defines};
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
fn bundled_esp_idf_version_is_read_from_legacy_sdk_header() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    let header = root.join("tools/sdk/esp32s3/include/esp_common/include/esp_idf_version.h");
    std::fs::create_dir_all(header.parent().unwrap()).unwrap();
    std::fs::write(&header, "#define ESP_IDF_VERSION_MAJOR 4\n#define ESP_IDF_VERSION_MINOR 4\n#define ESP_IDF_VERSION_PATCH 7\n").unwrap();
    let mut fw = Esp32Framework::new(root, "esp32s3");
    fw.install_dir = Some(root.to_path_buf());
    assert_eq!(
        fw.bundled_esp_idf_version("esp32s3").as_deref(),
        Some("4.4.7")
    );
}

#[test]
fn bundled_esp_idf_version_is_read_from_split_sdk_header() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    let header =
        root.join("tools/esp32-arduino-libs/esp32s3/include/esp_common/include/esp_idf_version.h");
    std::fs::create_dir_all(header.parent().unwrap()).unwrap();
    std::fs::write(&header, "#define ESP_IDF_VERSION_MAJOR 5\n#define ESP_IDF_VERSION_MINOR 5\n#define ESP_IDF_VERSION_PATCH 1\n").unwrap();
    let mut fw = Esp32Framework::new(root, "esp32s3");
    fw.install_dir = Some(root.to_path_buf());
    assert_eq!(
        fw.bundled_esp_idf_version("esp32s3").as_deref(),
        Some("5.5.1")
    );
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
    assert!(
        dirs.iter()
            .any(|d| d.ends_with(Path::new("dio_qspi").join("include")))
    );
    assert!(
        !dirs
            .iter()
            .any(|d| d.ends_with(Path::new("qio_opi").join("include")))
    );
}

#[test]
fn old_sdk_keeps_newlib_platform_headers_first() {
    let tmp = tempfile::TempDir::new().unwrap();
    let include = tmp.path().join("tools/sdk/esp32s3/include");
    let newlib = include.join("newlib/platform_include");
    let esp = include.join("esp_hw_support/include");
    std::fs::create_dir_all(&newlib).unwrap();
    std::fs::create_dir_all(&esp).unwrap();
    std::fs::write(newlib.join("assert.h"), "#define assert(x) ((void)0)\n").unwrap();
    std::fs::create_dir_all(newlib.join("sys")).unwrap();
    std::fs::write(newlib.join("sys/time.h"), "\n").unwrap();
    std::fs::write(esp.join("soc.h"), "\n").unwrap();
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

    let dirs = fw.get_sdk_include_dirs("esp32s3", None);
    assert_eq!(dirs.first(), Some(&newlib));
    assert!(!dirs.contains(&newlib.join("sys")));
}

#[test]
fn old_sdk_does_not_mix_rom_headers_from_other_mcus() {
    let tmp = tempfile::TempDir::new().unwrap();
    let rom = tmp.path().join("tools/sdk/esp32s3/include/esp_rom/include");
    for chip in ["esp32", "esp32c3", "esp32s3"] {
        let dir = rom.join(chip).join("rom");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("gpio.h"), "\n").unwrap();
    }
    let linux = rom.join("linux/soc");
    std::fs::create_dir_all(&linux).unwrap();
    std::fs::write(linux.join("reset_reasons.h"), "\n").unwrap();
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

    let dirs = fw.get_sdk_include_dirs("esp32s3", None);
    assert!(dirs.iter().any(|dir| dir == &rom.join("esp32s3")));
    assert!(!dirs.iter().any(|dir| dir == &rom.join("esp32")));
    assert!(!dirs.iter().any(|dir| dir == &rom.join("esp32c3")));
    assert!(!dirs.iter().any(|dir| dir == &linux));
}

#[test]
fn old_sdk_prefers_common_component_headers_before_chip_extensions() {
    let tmp = tempfile::TempDir::new().unwrap();
    let efuse = tmp.path().join("tools/sdk/esp32s3/include/efuse");
    let common = efuse.join("include");
    let chip = efuse.join("esp32s3/include");
    std::fs::create_dir_all(&common).unwrap();
    std::fs::create_dir_all(&chip).unwrap();
    std::fs::write(common.join("esp_efuse.h"), "\n").unwrap();
    std::fs::write(chip.join("esp_efuse_table.h"), "\n").unwrap();
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
    let dirs = fw.get_sdk_include_dirs("esp32s3", None);
    let common_pos = dirs.iter().position(|dir| dir == &common).unwrap();
    let chip_pos = dirs.iter().position(|dir| dir == &chip).unwrap();
    assert!(common_pos < chip_pos);
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
    assert!(
        flags
            .iter()
            .any(|f| f.ends_with("\\esp32s3\\dio_qspi") || f.ends_with("/esp32s3/dio_qspi"))
    );
    assert!(
        !flags
            .iter()
            .any(|f| f.ends_with("\\esp32s3\\qio_opi") || f.ends_with("/esp32s3/qio_opi"))
    );
}

#[test]
fn old_sdk_linker_scripts_search_selected_memory_variant() {
    let tmp = tempfile::TempDir::new().unwrap();
    let sdk = tmp.path().join("tools").join("sdk").join("esp32s3");
    std::fs::create_dir_all(sdk.join("ld")).unwrap();
    std::fs::create_dir_all(sdk.join("dio_qspi")).unwrap();
    std::fs::write(sdk.join("dio_qspi/sections.ld"), "\n").unwrap();
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
    let flags = fw.get_sdk_ld_scripts("esp32s3", Some("dio_qspi"));
    assert!(flags.contains(&format!("-L{}", sdk.join("dio_qspi").display())));
}

#[test]
fn old_sdk_libraries_use_selected_variant_before_common_archives() {
    let tmp = tempfile::TempDir::new().unwrap();
    let sdk = tmp.path().join("tools").join("sdk").join("esp32s3");
    let variant = sdk.join("qio_qspi");
    let common = sdk.join("lib");
    std::fs::create_dir_all(&variant).unwrap();
    std::fs::create_dir_all(&common).unwrap();
    std::fs::write(variant.join("libfreertos.a"), "").unwrap();
    std::fs::write(common.join("libfreertos.a"), "").unwrap();
    std::fs::write(common.join("libesp_system.a"), "").unwrap();
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
    let flags = fw.get_sdk_lib_flags("esp32s3", Some("qio_qspi"));
    assert_eq!(flags[0], format!("-L{}", variant.display()));
    assert_eq!(flags[1], format!("-L{}", common.display()));
    assert_eq!(flags.iter().filter(|flag| *flag == "-lfreertos").count(), 1);
    assert!(flags.contains(&"-lesp_system".to_string()));
}

#[test]
fn old_sdk_radio_archives_are_linked_from_ld_directory() {
    let tmp = tempfile::TempDir::new().unwrap();
    let sdk = tmp.path().join("tools").join("sdk").join("esp32");
    let common = sdk.join("lib");
    let ld = sdk.join("ld");
    std::fs::create_dir_all(&common).unwrap();
    std::fs::create_dir_all(&ld).unwrap();
    std::fs::write(common.join("libesp_phy.a"), "").unwrap();
    std::fs::write(ld.join("libphy.a"), "").unwrap();
    std::fs::write(ld.join("librtc.a"), "").unwrap();
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
    let flags = fw.get_sdk_lib_flags("esp32", None);
    assert!(flags.contains(&format!("-L{}", ld.display())));
    assert!(flags.contains(&"-lphy".to_string()));
    assert!(flags.contains(&"-lrtc".to_string()));
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

#[test]
fn old_sdk_uses_pio_builder_include_list_over_tree_scan() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    // A tree scan would find the decoy (a header dir under include/) and miss
    // the deeply nested leaf that only the builder script names.
    let decoy = root.join("tools/sdk/esp32s3/include/decoy/include");
    std::fs::create_dir_all(&decoy).unwrap();
    std::fs::write(decoy.join("decoy.h"), "\n").unwrap();

    let leaf = root.join("tools/sdk/esp32s3/include/bt/common/api/include/api");
    std::fs::create_dir_all(&leaf).unwrap();
    std::fs::write(leaf.join("esp_bt.h"), "\n").unwrap();
    let deep = root.join("tools/sdk/esp32s3/include/lwip/port/esp32/include");
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(deep.join("lwipopts.h"), "\n").unwrap();

    std::fs::create_dir_all(root.join("tools")).unwrap();
    let mut script = String::from("env.Append(\n    CPPPATH=[\n");
    for i in 0..25 {
        let rel = format!("tools/sdk/esp32s3/include/comp{i}/include");
        std::fs::create_dir_all(root.join(&rel)).unwrap();
        std::fs::write(root.join(&rel).join("h.h"), "\n").unwrap();
        script.push_str(&format!("        join(FRAMEWORK_DIR, \"tools\", \"sdk\", \"esp32s3\", \"include\", \"comp{i}\", \"include\"),\n"));
    }
    script.push_str("        join(FRAMEWORK_DIR, \"tools\", \"sdk\", \"esp32s3\", \"include\", \"bt\", \"common\", \"api\", \"include\", \"api\"),\n");
    script.push_str("        join(FRAMEWORK_DIR, \"tools\", \"sdk\", \"esp32s3\", \"include\", \"lwip\", \"port\", \"esp32\", \"include\"),\n");
    script.push_str("        join(FRAMEWORK_DIR, \"tools\", \"sdk\", \"esp32s3\", env.BoardConfig().get(\"build.flash_mode\"), \"include\"),\n");
    script.push_str("        join(FRAMEWORK_DIR, \"cores\", env.BoardConfig().get(\"build.core\"))\n    ],\n)\n");
    std::fs::write(root.join("tools/platformio-build-esp32s3.py"), script).unwrap();

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

    let dirs = fw.get_sdk_include_dirs("esp32s3", None);
    assert!(
        dirs.contains(&deep),
        "deep leaf from builder script missing"
    );
    assert!(
        dirs.contains(&leaf),
        "nested leaf from builder script missing"
    );
    assert!(
        !dirs.contains(&decoy),
        "tree scan decoy leaked into the builder-script list"
    );
    // Order follows the script, not a sort: the script names `bt` before `lwip`.
    assert!(dirs.iter().position(|d| d == &leaf) < dirs.iter().position(|d| d == &deep));
}

#[test]
fn old_sdk_falls_back_to_tree_scan_when_builder_script_unparseable() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    let include = root.join("tools/sdk/esp32s3/include/efuse/include");
    std::fs::create_dir_all(&include).unwrap();
    std::fs::write(include.join("esp_efuse.h"), "\n").unwrap();
    std::fs::create_dir_all(root.join("tools")).unwrap();
    // Truncated/substituted CPPPATH that yields too few entries to trust.
    std::fs::write(
        root.join("tools/platformio-build-esp32s3.py"),
        "env.Append(\n    CPPPATH=[\n        join(FRAMEWORK_DIR, \"cores\")\n    ],\n)\n",
    )
    .unwrap();

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

    let dirs = fw.get_sdk_include_dirs("esp32s3", None);
    assert!(dirs.contains(&include), "tree-scan fallback did not run");
}

#[test]
fn idf44_sdk_includes_qspi_qspi_sdkconfig() {
    let tmp = tempfile::TempDir::new().unwrap();
    let sdk_config = tmp
        .path()
        .join("tools/sdk/esp32/qspi_qspi/include/sdkconfig.h");
    std::fs::create_dir_all(sdk_config.parent().unwrap()).unwrap();
    std::fs::write(&sdk_config, "\n").unwrap();
    let sdk_include = tmp.path().join("tools/sdk/esp32/include/freertos/include");
    std::fs::create_dir_all(&sdk_include).unwrap();
    std::fs::write(sdk_include.join("FreeRTOS.h"), "\n").unwrap();

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

    let dirs = fw.get_sdk_include_dirs("esp32", None);
    assert!(
        dirs.contains(&sdk_config.parent().unwrap().to_path_buf()),
        "ESP-IDF 4.4 SDK configuration include directory missing"
    );
}

#[test]
fn old_sdk_rejects_builder_script_padded_with_duplicate_entries() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    // A dir only the tree scan would find, used to prove the fallback ran.
    let scanned = root.join("tools/sdk/esp32s3/include/efuse/include");
    std::fs::create_dir_all(&scanned).unwrap();
    std::fs::write(scanned.join("esp_efuse.h"), "\n").unwrap();

    let dup = root.join("tools/sdk/esp32s3/include/only/include");
    std::fs::create_dir_all(&dup).unwrap();
    std::fs::write(dup.join("only.h"), "\n").unwrap();

    std::fs::create_dir_all(root.join("tools")).unwrap();
    // Same entry repeated past the minimum: distinct-path count is 1, so this
    // must not be trusted even though the line count clears the threshold.
    let mut script = String::from("env.Append(\n    CPPPATH=[\n");
    for _ in 0..30 {
        script.push_str(
            "        join(FRAMEWORK_DIR, \"tools\", \"sdk\", \"esp32s3\", \"include\", \"only\", \"include\"),\n",
        );
    }
    script.push_str("    ],\n)\n");
    std::fs::write(root.join("tools/platformio-build-esp32s3.py"), script).unwrap();

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

    let dirs = fw.get_sdk_include_dirs("esp32s3", None);
    assert!(
        dirs.contains(&scanned),
        "duplicate-padded script should have been rejected in favour of the tree scan"
    );
}

/// The `CPPDEFINES` block of arduino-esp32 2.x `platformio-build-esp32s3.py`.
const PIO_BUILD_SCRIPT: &str = r#"
env.Append(
    CPPDEFINES=[
        "HAVE_CONFIG_H",
        ("MBEDTLS_CONFIG_FILE", '\\"mbedtls/esp_config.h\\"'),
        "UNITY_INCLUDE_CONFIG_H",
        "WITH_POSIX",
        "_GNU_SOURCE",
        ("IDF_VER", '\\"v4.4.7-dirty\\"'),
        "ESP_PLATFORM",
        "_POSIX_READER_WRITER_LOCKS",
        "ARDUINO_ARCH_ESP32",
        "ESP32",
        ("F_CPU", "$BOARD_F_CPU"),
        ("ARDUINO", 10812),
        ("ARDUINO_VARIANT", '\\"%s\\"' % env.BoardConfig().get("build.variant").replace('"', "")),
        "ARDUINO_PARTITION_%s" % basename(env.BoardConfig().get(
            "build.partitions", "default.csv")).replace(".csv", "").replace("-", "_")
    ]
)
"#;

#[test]
fn pio_cppdefines_yield_the_sdk_defines_in_flags_defines_form() {
    assert_eq!(
        parse_pio_cppdefines(PIO_BUILD_SCRIPT),
        [
            "-DHAVE_CONFIG_H",
            r#"-DMBEDTLS_CONFIG_FILE=\"mbedtls/esp_config.h\""#,
            "-DUNITY_INCLUDE_CONFIG_H",
            "-DWITH_POSIX",
            "-D_GNU_SOURCE",
            r#"-DIDF_VER=\"v4.4.7-dirty\""#,
            "-DESP_PLATFORM",
            "-D_POSIX_READER_WRITER_LOCKS",
        ]
    );
}

#[test]
fn sdk_defines_fall_back_to_the_pio_build_script_without_flags_defines() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("tools/sdk/esp32s3")).unwrap();
    std::fs::write(
        root.join("tools/platformio-build-esp32s3.py"),
        PIO_BUILD_SCRIPT,
    )
    .unwrap();
    let mut fw = Esp32Framework::new(root, "esp32s3");
    fw.install_dir = Some(root.to_path_buf());

    let defines = fw.get_sdk_defines("esp32s3");

    assert!(defines.contains(&"-DESP_PLATFORM".to_string()));
    assert_eq!(defines.len(), 8);
}

#[test]
fn sdk_defines_prefer_flags_defines_over_the_pio_build_script() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("tools/sdk/esp32s3/flags")).unwrap();
    std::fs::write(root.join("tools/sdk/esp32s3/flags/defines"), "-DFROM_FLAGS").unwrap();
    std::fs::write(
        root.join("tools/platformio-build-esp32s3.py"),
        PIO_BUILD_SCRIPT,
    )
    .unwrap();
    let mut fw = Esp32Framework::new(root, "esp32s3");
    fw.install_dir = Some(root.to_path_buf());

    assert_eq!(fw.get_sdk_defines("esp32s3"), ["-DFROM_FLAGS"]);
}

/// A board whose `build.chip_variant` differs from `build.mcu` (ESP32-P4 ES)
/// links against `esp32-arduino-libs/<chip_variant>/`. The install check must
/// require BOTH trees: a per-chip libs slice supplies only the `mcu` one, so a
/// "present" verdict sent the build into a missing SDK dir and it died on
/// `soc/soc_caps.h` (FastLED/fbuild#1897).
#[test]
fn sdk_libs_installed_requires_the_board_chip_variant_tree() {
    let tmp = tempfile::TempDir::new().unwrap();
    let sdk_dir = tmp.path().join("tools").join("esp32-arduino-libs");
    let seed = |dir: &std::path::Path| {
        let kernel = dir
            .join("include")
            .join("freertos")
            .join("FreeRTOS-Kernel")
            .join("include")
            .join("freertos")
            .join("FreeRTOS.h");
        std::fs::create_dir_all(kernel.parent().unwrap()).unwrap();
        std::fs::write(&kernel, "").unwrap();
        std::fs::create_dir_all(dir.join("flags")).unwrap();
        std::fs::write(dir.join("flags").join("includes"), "").unwrap();
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::write(dir.join("lib").join("libfreertos.a"), "").unwrap();
    };
    seed(&sdk_dir.join("esp32p4"));

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

    assert!(fw.sdk_libs_installed("esp32p4", "esp32p4"));
    assert!(!fw.sdk_libs_installed("esp32p4", "esp32p4_es"));

    seed(&sdk_dir.join("esp32p4_es"));
    assert!(fw.sdk_libs_installed("esp32p4", "esp32p4_es"));
}

#[test]
fn test_core_component_identity_is_stamped_at_the_framework_root() {
    use fbuild_packages_fetch::component_id::read_stamp;

    let tmp = tempfile::TempDir::new().unwrap();
    let staging = tmp.path();
    // A framework that has already had its SDK libraries merged into tools/, the
    // way the post-install merge leaves it. Only the core half may be identified.
    std::fs::create_dir_all(staging.join("cores/esp32")).unwrap();
    std::fs::write(staging.join("cores/esp32/Arduino.h"), b"#pragma once").unwrap();
    std::fs::create_dir_all(staging.join("tools/esp32-arduino-libs/esp32s3/lib")).unwrap();
    std::fs::write(
        staging.join("tools/esp32-arduino-libs/esp32s3/lib/libesp32.a"),
        b"sdk",
    )
    .unwrap();

    super::stamp_core_component(staging, "3.3.11").unwrap();

    let identity = read_stamp(staging, "core").expect("the core root must carry a stamp");
    assert_eq!(identity.component, "core");
    assert_eq!(identity.version, "3.3.11");
    assert_eq!(identity.recipe, super::CORE_RECIPE);
    assert_eq!(identity.content_id.len(), 64);

    // The SDK merge writes into tools/ after the core install commits, so the
    // core identity must not move when it does.
    let before = identity.content_id;
    std::fs::create_dir_all(staging.join("tools/esp32-arduino-libs/esp32c6/lib")).unwrap();
    std::fs::write(
        staging.join("tools/esp32-arduino-libs/esp32c6/lib/libesp32.a"),
        b"other chip",
    )
    .unwrap();
    super::stamp_core_component(staging, "3.3.11").unwrap();
    assert_eq!(read_stamp(staging, "core").unwrap().content_id, before);
}

// --- MCU skeleton component identity (decision 8a, FastLED/fbuild#1882) ---

fn skeleton_extract(root: &Path, wrapper: &str, mcu: &str) {
    let dir = root.join(wrapper).join(mcu);
    std::fs::create_dir_all(dir.join("flags")).unwrap();
    std::fs::write(dir.join("flags/includes"), "-Iinclude\n").unwrap();
    std::fs::create_dir_all(dir.join("include/hal/include/hal")).unwrap();
    std::fs::write(dir.join("include/hal/include/hal/hal.h"), "//hal\n").unwrap();
}

#[test]
fn skeleton_identity_is_taken_in_staging_and_survives_the_merge() {
    use fbuild_packages_fetch::component_id::{content_id, read_stamp};
    let tmp = tempfile::TempDir::new().unwrap();
    let staging = tmp.path().join("staging");
    let tools = tmp.path().join("tools");
    std::fs::create_dir_all(&tools).unwrap();
    skeleton_extract(&staging, "c2_arduino_compile_skeleton", "esp32c2");

    let merged = super::libs::install_skeleton_from_archive(
        &staging,
        &tools,
        "esp32c2",
        "c2_arduino_compile_skeleton",
    )
    .unwrap();
    assert!(merged);

    let installed = tools.join("esp32-arduino-libs").join("esp32c2");
    let identity =
        read_stamp(&installed, "mcu-esp32c2").expect("the merged skeleton must carry its stamp");
    assert_eq!(identity.component, "mcu-esp32c2");
    assert_eq!(identity.version, "c2_arduino_compile_skeleton");
    assert_eq!(identity.recipe, super::libs::MCU_SKELETON_RECIPE);
    // The compat patch ran before the stamp, so the header it writes is inside
    // the identity rather than a post-stamp mutation of the installed tree.
    assert!(
        installed
            .join("include/hal/include/hal/touch_sensor_legacy_types.h")
            .exists()
    );
    assert_eq!(
        identity.content_id,
        content_id(&installed, &[]).unwrap(),
        "the merged tree must reproduce the staged bytes"
    );
}

#[test]
fn skeleton_identity_survives_a_preexisting_partial_tree_at_the_destination() {
    // ESP32-C2 in Arduino-ESP32 3.3.x: the core archive ships a partial
    // tools/esp32-arduino-libs/esp32c2/ that lacks FreeRTOS, so the skeleton
    // merge always lands next to files the skeleton did not contribute. Whole-tree
    // equality would withdraw the identity on the one MCU skeletons exist for.
    use fbuild_packages_fetch::component_id::read_stamp;
    let tmp = tempfile::TempDir::new().unwrap();
    let staging = tmp.path().join("staging");
    let tools = tmp.path().join("tools");
    let installed = tools.join("esp32-arduino-libs").join("esp32c2");
    std::fs::create_dir_all(&installed).unwrap();
    std::fs::write(installed.join("leftover-from-core-archive.txt"), b"partial").unwrap();
    skeleton_extract(&staging, "skeleton", "esp32c2");

    super::libs::install_skeleton_from_archive(&staging, &tools, "esp32c2", "skeleton").unwrap();

    assert!(
        read_stamp(&installed, "mcu-esp32c2").is_some(),
        "the entries the skeleton contributed did survive the merge"
    );
    assert!(installed.join("leftover-from-core-archive.txt").exists());
}

#[test]
fn a_skeleton_merge_keeps_the_sdk_variant_identity_that_shares_its_directory() {
    // The SDK variant of a chip and its MCU skeleton install into the same
    // directory, so the skeleton's stamp must not overwrite the SDK's.
    use fbuild_packages_fetch::component_id::{
        ComponentIdentity, content_id, read_stamp, write_stamp,
    };
    let tmp = tempfile::TempDir::new().unwrap();
    let staging = tmp.path().join("staging");
    let tools = tmp.path().join("tools");
    let installed = tools.join("esp32-arduino-libs").join("esp32c2");
    std::fs::create_dir_all(installed.join("lib")).unwrap();
    std::fs::write(installed.join("lib/libdriver.a"), "driver").unwrap();
    let sdk_id = content_id(&installed, &[]).unwrap();
    write_stamp(
        &installed,
        &ComponentIdentity::new("sdk-esp32c2", "3.3.12", sdk_id.clone(), 1),
    )
    .unwrap();

    skeleton_extract(&staging, "skeleton", "esp32c2");
    super::libs::install_skeleton_from_archive(&staging, &tools, "esp32c2", "skeleton").unwrap();

    assert_eq!(
        read_stamp(&installed, "sdk-esp32c2").map(|stamp| stamp.content_id),
        Some(sdk_id),
        "the SDK identity must survive a skeleton merge into the same directory"
    );
    assert!(read_stamp(&installed, "mcu-esp32c2").is_some());
}

#[test]
fn skeleton_archive_without_the_requested_mcu_carries_no_identity() {
    use fbuild_packages_fetch::component_id::read_stamp;
    let tmp = tempfile::TempDir::new().unwrap();
    let staging = tmp.path().join("staging");
    let tools = tmp.path().join("tools");
    std::fs::create_dir_all(&tools).unwrap();
    skeleton_extract(&staging, "skeleton", "esp32c6");

    let merged =
        super::libs::install_skeleton_from_archive(&staging, &tools, "esp32c2", "skeleton")
            .unwrap();

    assert!(!merged);
    assert!(!tools.join("esp32-arduino-libs").join("esp32c2").exists());
    assert!(read_stamp(&staging, "mcu-esp32c2").is_none());
}

#[test]
fn a_stamped_tree_is_not_patched_in_place() {
    use fbuild_packages_fetch::component_id::{
        ComponentIdentity, content_id, read_stamp, write_stamp,
    };
    let tmp = tempfile::TempDir::new().unwrap();
    let mcu_dir = tmp.path().join("esp32c2");
    std::fs::create_dir_all(mcu_dir.join("include")).unwrap();
    std::fs::write(mcu_dir.join("include/hal.h"), "//hal\n").unwrap();
    let stamped = read_stamp(&mcu_dir, "mcu-esp32c2");
    assert!(stamped.is_none());
    // An unstamped (legacy) tree is patched in place and stays identity-less.
    super::libs::patch_unless_stamped(&mcu_dir, "esp32c2").unwrap();
    assert!(
        mcu_dir
            .join("include/hal/include/hal/touch_sensor_legacy_types.h")
            .exists()
    );
    assert!(read_stamp(&mcu_dir, "mcu-esp32c2").is_none());

    // The same tree, stamped: the patch must not run.
    let id = content_id(&mcu_dir, &[]).unwrap();
    write_stamp(
        &mcu_dir,
        &ComponentIdentity::new(
            "mcu-esp32c2",
            "skeleton",
            id,
            super::libs::MCU_SKELETON_RECIPE,
        ),
    )
    .unwrap();
    std::fs::remove_file(mcu_dir.join("include/hal/include/hal/touch_sensor_legacy_types.h"))
        .unwrap();
    super::libs::patch_unless_stamped(&mcu_dir, "esp32c2").unwrap();
    assert!(
        !mcu_dir
            .join("include/hal/include/hal/touch_sensor_legacy_types.h")
            .exists(),
        "a stamped tree must never be mutated after stamping"
    );
}
