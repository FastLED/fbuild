//! Unit tests for the board module.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use tempfile::NamedTempFile;

use super::db::get_board_db;
use super::loaders::parse_boards_txt;
use super::{BoardConfig, Esp32QemuPsramConfig};

fn write_boards_txt(content: &str) -> NamedTempFile {
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(content.as_bytes()).unwrap();
    f.flush().unwrap();
    f
}

#[test]
fn test_from_board_id_uno() {
    let config = BoardConfig::from_board_id("uno", &HashMap::new()).unwrap();
    assert_eq!(config.name, "Arduino Uno");
    assert_eq!(config.mcu, "atmega328p");
    assert_eq!(config.f_cpu, "16000000L");
    assert_eq!(config.board, "UNO");
    assert_eq!(config.core, "arduino");
    assert_eq!(config.variant, "standard");
    assert_eq!(config.max_flash, Some(32256));
    assert_eq!(config.max_ram, Some(2048));
    // extra_flags from enriched JSON
    assert_eq!(config.extra_flags, Some("-DARDUINO_AVR_UNO".to_string()));
    assert_eq!(config.upload_protocol, Some("arduino".to_string()));
    assert_eq!(config.upload_speed, Some("115200".to_string()));
}

#[test]
fn test_from_board_id_mega() {
    let config = BoardConfig::from_board_id("mega", &HashMap::new()).unwrap();
    assert_eq!(config.mcu, "atmega2560");
    assert_eq!(config.variant, "mega");
}

#[test]
fn test_from_board_id_megaatmega2560_alias() {
    let config = BoardConfig::from_board_id("megaatmega2560", &HashMap::new()).unwrap();
    assert_eq!(config.mcu, "atmega2560");
}

#[test]
fn test_ch32v_aliases() {
    let cases = [
        ("ch32l103", "ch32l103c8t6", Some("variant_CH32L103C8T6.h")),
        ("ch32v003", "ch32v003f4p6", Some("variant_CH32V003F4.h")),
        ("ch32v006", "ch32v006k8u6", Some("variant_CH32V006K8.h")),
        ("ch32v103", "ch32v103c8t6", Some("variant_CH32V103R8T6.h")),
        ("ch32v203", "ch32v203c8t6", Some("variant_CH32V203C8.h")),
        ("ch32v208", "ch32v208wbu6", Some("variant_CH32V203C8.h")),
        ("ch32v303", "ch32v303vct6", Some("variant_CH32V307VCT6.h")),
        ("ch32v307", "ch32v307vct6", Some("variant_CH32V307VCT6.h")),
        ("ch32x035", "ch32x035c8t6", Some("variant_CH32X035G8U.h")),
    ];
    for (alias, expected_mcu, expected_variant_h) in cases {
        let config = BoardConfig::from_board_id(alias, &HashMap::new()).unwrap();
        assert_eq!(
            config.mcu, expected_mcu,
            "alias '{}' should resolve to mcu '{}'",
            alias, expected_mcu
        );
        assert_eq!(
            config.variant_h.as_deref(),
            expected_variant_h,
            "alias '{}' should resolve to variant_h '{:?}'",
            alias,
            expected_variant_h
        );
    }
}

#[test]
fn test_from_board_id_unknown() {
    let result = BoardConfig::from_board_id("nonexistent_board", &HashMap::new());
    assert!(result.is_err());
}

#[test]
fn test_from_board_id_with_overrides() {
    let mut overrides = HashMap::new();
    overrides.insert("f_cpu".to_string(), "8000000L".to_string());
    let config = BoardConfig::from_board_id("uno", &overrides).unwrap();
    assert_eq!(config.f_cpu, "8000000L");
    assert_eq!(config.mcu, "atmega328p"); // not overridden
}

#[test]
fn test_from_boards_txt_valid() {
    let f = write_boards_txt(
        "\
uno.name=Arduino Uno
uno.build.mcu=atmega328p
uno.build.f_cpu=16000000L
uno.build.board=AVR_UNO
uno.build.core=arduino
uno.build.variant=standard
uno.upload.maximum_size=32256
uno.upload.maximum_data_size=2048
",
    );
    let config = BoardConfig::from_boards_txt(f.path(), "uno", &HashMap::new()).unwrap();
    assert_eq!(config.name, "Arduino Uno");
    assert_eq!(config.mcu, "atmega328p");
    assert_eq!(config.max_flash, Some(32256));
}

#[test]
fn test_from_boards_txt_nonexistent_board() {
    let f = write_boards_txt("uno.name=Arduino Uno\nuno.build.mcu=atmega328p\n");
    let result = BoardConfig::from_boards_txt(f.path(), "mega", &HashMap::new());
    assert!(result.is_err());
}

#[test]
fn test_from_boards_txt_with_overrides() {
    let f = write_boards_txt(
        "\
uno.name=Arduino Uno
uno.build.mcu=atmega328p
uno.build.f_cpu=16000000L
uno.build.board=AVR_UNO
uno.build.core=arduino
uno.build.variant=standard
",
    );
    let mut overrides = HashMap::new();
    overrides.insert("f_cpu".to_string(), "8000000L".to_string());
    let config = BoardConfig::from_boards_txt(f.path(), "uno", &overrides).unwrap();
    assert_eq!(config.f_cpu, "8000000L");
}

#[test]
fn test_from_boards_txt_multiple_boards() {
    let f = write_boards_txt(
        "\
uno.name=Arduino Uno
uno.build.mcu=atmega328p
mega.name=Arduino Mega 2560
mega.build.mcu=atmega2560
",
    );
    let uno = BoardConfig::from_boards_txt(f.path(), "uno", &HashMap::new()).unwrap();
    let mega = BoardConfig::from_boards_txt(f.path(), "mega", &HashMap::new()).unwrap();
    assert_eq!(uno.mcu, "atmega328p");
    assert_eq!(mega.mcu, "atmega2560");
}

#[test]
fn test_from_boards_txt_with_upload_info() {
    let f = write_boards_txt(
        "\
leonardo.name=Arduino Leonardo
leonardo.build.mcu=atmega32u4
leonardo.build.vid=0x2341
leonardo.build.pid=0x8036
leonardo.upload.protocol=avr109
leonardo.upload.speed=57600
",
    );
    let config = BoardConfig::from_boards_txt(f.path(), "leonardo", &HashMap::new()).unwrap();
    assert_eq!(config.vid, Some("0x2341".to_string()));
    assert_eq!(config.pid, Some("0x8036".to_string()));
    assert_eq!(config.upload_protocol, Some("avr109".to_string()));
    assert_eq!(config.upload_speed, Some("57600".to_string()));
}

#[test]
fn test_get_defines_basic() {
    let config = BoardConfig::from_board_id("uno", &HashMap::new()).unwrap();
    let defines = config.get_defines();
    assert_eq!(defines.get("PLATFORMIO"), Some(&"1".to_string()));
    assert_eq!(defines.get("F_CPU"), Some(&"16000000L".to_string()));
    assert_eq!(defines.get("ARDUINO"), Some(&"10808".to_string()));
    assert_eq!(defines.get("ARDUINO_AVR_UNO"), Some(&"1".to_string()));
    assert_eq!(defines.get("ARDUINO_ARCH_AVR"), Some(&"1".to_string()));
    assert_eq!(defines.get("__AVR_ATMEGA328P__"), Some(&"1".to_string()));
}

#[test]
fn test_get_defines_with_extra_flags() {
    let mut config = BoardConfig::from_board_id("uno", &HashMap::new()).unwrap();
    config.extra_flags = Some("-DCUSTOM_FLAG -DVALUE=42".to_string());
    let defines = config.get_defines();
    assert_eq!(defines.get("CUSTOM_FLAG"), Some(&"1".to_string()));
    assert_eq!(defines.get("VALUE"), Some(&"42".to_string()));
}

#[test]
fn test_get_include_paths() {
    let config = BoardConfig::from_board_id("uno", &HashMap::new()).unwrap();
    let paths = config.get_include_paths(Path::new("/framework"));
    assert_eq!(paths.len(), 2);
    assert_eq!(paths[0], PathBuf::from("/framework/cores/arduino"));
    assert_eq!(paths[1], PathBuf::from("/framework/variants/standard"));
}

#[test]
fn test_platform_detection() {
    let avr = BoardConfig::from_board_id("uno", &HashMap::new()).unwrap();
    assert_eq!(avr.platform(), Some(fbuild_core::Platform::AtmelAvr));

    let esp = BoardConfig::from_board_id("esp32dev", &HashMap::new()).unwrap();
    assert_eq!(esp.platform(), Some(fbuild_core::Platform::Espressif32));

    let teensy = BoardConfig::from_board_id("teensy41", &HashMap::new()).unwrap();
    assert_eq!(teensy.platform(), Some(fbuild_core::Platform::Teensy));

    let rpi = BoardConfig::from_board_id("rpipico", &HashMap::new()).unwrap();
    assert_eq!(rpi.platform(), Some(fbuild_core::Platform::RaspberryPi));
}

#[test]
fn test_parse_boards_txt_with_comments() {
    let props = parse_boards_txt(
        "# This is a comment\nuno.name=Arduino Uno\n# Another comment\nuno.build.mcu=atmega328p\n",
        "uno",
    );
    assert_eq!(props.get("name"), Some(&"Arduino Uno".to_string()));
    assert_eq!(props.get("mcu"), Some(&"atmega328p".to_string()));
}

#[test]
fn test_parse_boards_txt_empty_lines() {
    let props = parse_boards_txt(
        "\nuno.name=Arduino Uno\n\nuno.build.mcu=atmega328p\n\n",
        "uno",
    );
    assert_eq!(props.get("name"), Some(&"Arduino Uno".to_string()));
}

#[test]
fn test_parse_boards_txt_ignores_other_boards() {
    let props = parse_boards_txt(
        "uno.name=Arduino Uno\nmega.name=Arduino Mega\nuno.build.mcu=atmega328p\n",
        "uno",
    );
    assert_eq!(props.get("name"), Some(&"Arduino Uno".to_string()));
    assert!(!props.values().any(|v| v == "Arduino Mega"));
}

#[test]
fn test_repr() {
    let config = BoardConfig::from_board_id("uno", &HashMap::new()).unwrap();
    let repr = format!("{:?}", config);
    assert!(repr.contains("Arduino Uno"));
    assert!(repr.contains("atmega328p"));
}

// --- Data-driven enriched JSON tests ---

#[test]
fn test_mega_upload_protocol_wiring() {
    // Bug fix: mega needs protocol=wiring, not arduino
    let config = BoardConfig::from_board_id("mega", &HashMap::new()).unwrap();
    assert_eq!(config.upload_protocol, Some("wiring".to_string()));
    assert_eq!(config.core, "arduino");
    assert_eq!(config.variant, "mega");
}

#[test]
fn test_nano_upload_speed_57600() {
    // Bug fix: nano uses 57600, not 115200
    let config = BoardConfig::from_board_id("nano", &HashMap::new()).unwrap();
    assert_eq!(config.upload_speed, Some("57600".to_string()));
    assert_eq!(config.upload_protocol, Some("arduino".to_string()));
    assert_eq!(config.variant, "eightanaloginputs");
}

#[test]
fn test_teensy36_core_teensy3() {
    // Bug fix: teensy30-36 use core=teensy3, not teensy4
    let config = BoardConfig::from_board_id("teensy36", &HashMap::new()).unwrap();
    assert_eq!(config.core, "teensy3");
    assert_eq!(config.upload_protocol, Some("teensy-gui".to_string()));
}

#[test]
fn test_teensy41_core_teensy4() {
    let config = BoardConfig::from_board_id("teensy41", &HashMap::new()).unwrap();
    assert_eq!(config.core, "teensy4");
    assert_eq!(config.upload_protocol, Some("teensy-gui".to_string()));
}

#[test]
fn test_esp32dev_enriched_fields() {
    let config = BoardConfig::from_board_id("esp32dev", &HashMap::new()).unwrap();
    assert_eq!(config.core, "esp32");
    assert_eq!(config.variant, "esp32");
    // ESP32 boards now intentionally drop the JSON-shipped flash_mode
    // (see comments in `from_board_id`). Downstream consumers fall back
    // to mcu_config.default_flash_mode() which is "dio".
    assert_eq!(config.flash_mode, None);
    assert_eq!(config.memory_type, None);
    assert_eq!(config.f_flash, Some("40000000L".to_string()));
    assert_eq!(config.ldscript, Some("esp32_out.ld".to_string()));
    assert_eq!(config.upload_speed, Some("460800".to_string()));
}

#[test]
fn test_esp32_flash_mode_env_override_honoured() {
    // The user can opt back into QIO via `board_build.flash_mode = qio`
    // in their [env:X] section, which the daemon translates into a
    // `flash_mode` override key.
    let mut overrides = HashMap::new();
    overrides.insert("flash_mode".to_string(), "qio".to_string());
    let config = BoardConfig::from_board_id("esp32dev", &overrides).unwrap();
    assert_eq!(config.flash_mode, Some("qio".to_string()));
}

#[test]
fn test_pico_enriched_fields() {
    let config = BoardConfig::from_board_id("rpipico", &HashMap::new()).unwrap();
    assert_eq!(config.core, "earlephilhower");
    assert_eq!(config.variant, "rpipico");
    assert_eq!(config.upload_protocol, Some("picotool".to_string()));
}

#[test]
fn test_sparkfun_xrp_controller_board_config() {
    // SparkFun XRP Controller (RP2350B); maxgerhardt/platform-raspberrypi.
    // Regression for FastLED `rp2350B SparkfunXRP` workflow (#295).
    let config =
        BoardConfig::from_board_id("sparkfun_xrp_controller", &HashMap::new()).unwrap();
    assert_eq!(config.mcu, "rp2350");
    assert_eq!(config.core, "earlephilhower");
    assert_eq!(config.variant, "sparkfun_xrp_controller");
    assert_eq!(config.upload_protocol, Some("picotool".to_string()));
}

#[test]
fn test_extra_flags_produce_defines() {
    // Enriched extra_flags should produce correct ARDUINO_* defines
    let config = BoardConfig::from_board_id("uno", &HashMap::new()).unwrap();
    let defines = config.get_defines();
    assert_eq!(defines.get("ARDUINO_AVR_UNO"), Some(&"1".to_string()));
}

#[test]
fn test_esp32c3_board_config() {
    let config = BoardConfig::from_board_id("esp32c3", &HashMap::new()).unwrap();
    assert_eq!(config.mcu, "esp32c3");
    assert_eq!(config.core, "esp32");
    // ESP32 boards drop the JSON-shipped flash_mode (see
    // `test_esp32dev_enriched_fields`); fall back is "dio" from MCU config.
    assert_eq!(config.flash_mode, None);
    assert_eq!(config.memory_type, None);
    assert_eq!(config.ldscript, Some("esp32c3_out.ld".to_string()));
    // ESP32-C3 DevKit runs at 160 MHz
    assert_eq!(config.f_cpu, "160000000L");
}

#[test]
fn test_esp32_effective_memory_type_tracks_effective_flash_mode() {
    let config = BoardConfig::from_board_id("esp32c3", &HashMap::new()).unwrap();
    assert_eq!(
        config.effective_esp32_memory_type("dio"),
        Some("dio_qspi".to_string())
    );
}

#[test]
fn test_esp32p4_evboard_uses_es_chip_variant() {
    // The ESP32-P4 Function EV Board ships eco0–eco2 silicon ("ES pre rev.300").
    // It must link against the `esp32p4_es` SDK (base ROM), not `esp32p4` (eco5
    // ROM) — otherwise the bootloader panics on an illegal instruction.
    let config = BoardConfig::from_board_id("esp32-p4-evboard", &HashMap::new()).unwrap();
    assert_eq!(config.mcu, "esp32p4");
    assert_eq!(config.chip_variant, Some("esp32p4_es".to_string()));
    assert_eq!(config.sdk_variant(), "esp32p4_es");
}

#[test]
fn test_esp32p4_r3_uses_eco5_chip_variant() {
    // The rev.300 board targets eco5 silicon and links the `esp32p4` SDK.
    let config = BoardConfig::from_board_id("esp32-p4_r3", &HashMap::new()).unwrap();
    assert_eq!(config.chip_variant, Some("esp32p4".to_string()));
    assert_eq!(config.sdk_variant(), "esp32p4");
}

#[test]
fn test_sdk_variant_falls_back_to_mcu() {
    // Boards without an explicit chip_variant resolve the SDK dir from the MCU.
    let config = BoardConfig::from_board_id("esp32c3", &HashMap::new()).unwrap();
    assert_eq!(config.chip_variant, None);
    assert_eq!(config.sdk_variant(), "esp32c3");
}

#[test]
fn test_esp32_effective_memory_type_preserves_opi_flash_profiles() {
    let config = BoardConfig::from_board_id("esp32-s3-devkitc-1-n32r8v", &HashMap::new()).unwrap();
    assert_eq!(
        config.effective_esp32_memory_type("dio"),
        Some("opi_opi".to_string())
    );
}

#[test]
fn test_qemu_psram_config_absent_for_non_psram_board() {
    let config = BoardConfig::from_board_id("esp32-s3-devkitc-1", &HashMap::new()).unwrap();
    assert_eq!(config.qemu_esp32_psram_config(), None);
}

#[test]
fn test_qemu_psram_config_detects_quad_psram_board() {
    let config = BoardConfig::from_board_id("esp32-s3-devkitc1-n8r2", &HashMap::new()).unwrap();
    assert_eq!(
        config.qemu_esp32_psram_config(),
        Some(Esp32QemuPsramConfig {
            size_mib: 2,
            is_octal: false,
        })
    );
}

#[test]
fn test_qemu_psram_config_detects_octal_psram_board() {
    let config = BoardConfig::from_board_id("esp32-s3-devkitc1-n8r8", &HashMap::new()).unwrap();
    assert_eq!(
        config.qemu_esp32_psram_config(),
        Some(Esp32QemuPsramConfig {
            size_mib: 8,
            is_octal: true,
        })
    );
}

#[test]
fn test_esp32c3_devkitm1_board_config() {
    // Direct look up by full board ID (same underlying JSON)
    let config = BoardConfig::from_board_id("esp32-c3-devkitm-1", &HashMap::new()).unwrap();
    assert_eq!(config.mcu, "esp32c3");
    let flags = config.extra_flags.unwrap_or_default();
    assert!(
        flags.contains("ARDUINO_ESP32C3_DEV"),
        "expected ARDUINO_ESP32C3_DEV in extra_flags, got: {flags}"
    );
}

#[test]
fn test_esp32c3_no_psram() {
    // The plain C3 DevKit has no PSRAM
    let config = BoardConfig::from_board_id("esp32c3", &HashMap::new()).unwrap();
    let flags = config.extra_flags.clone().unwrap_or_default();
    assert!(
        !flags.contains("BOARD_HAS_PSRAM"),
        "ESP32-C3 DevKit should not have PSRAM flag, got: {flags}"
    );
}

// --- USB VID/PID defines ---

#[test]
fn test_get_defines_usb_vid_pid() {
    let mut config = BoardConfig::from_board_id("uno", &HashMap::new()).unwrap();
    config.vid = Some("0x2341".to_string());
    config.pid = Some("0x8036".to_string());
    let defines = config.get_defines();
    assert_eq!(defines.get("USB_VID"), Some(&"0x2341".to_string()));
    assert_eq!(defines.get("USB_PID"), Some(&"0x8036".to_string()));
}

#[test]
fn test_get_defines_no_usb_when_absent() {
    let config = BoardConfig::from_board_id("esp32dev", &HashMap::new()).unwrap();
    assert!(config.vid.is_none());
    let defines = config.get_defines();
    assert!(!defines.contains_key("USB_VID"));
    assert!(!defines.contains_key("USB_PID"));
}

#[test]
fn test_leonardo_board_has_vid_pid() {
    let config = BoardConfig::from_board_id("leonardo", &HashMap::new()).unwrap();
    assert_eq!(config.vid, Some("0x2341".to_string()));
    assert_eq!(config.pid, Some("0x8036".to_string()));
    let defines = config.get_defines();
    assert_eq!(defines.get("USB_VID"), Some(&"0x2341".to_string()));
    assert_eq!(defines.get("USB_PID"), Some(&"0x8036".to_string()));
}

#[test]
fn test_attiny1604_board_config() {
    let config = BoardConfig::from_board_id("ATtiny1604", &HashMap::new()).unwrap();
    assert_eq!(config.mcu, "attiny1604");
    assert_eq!(config.core, "megatinycore");
    assert_eq!(config.variant, "txy4");
    assert_eq!(config.platform(), Some(fbuild_core::Platform::AtmelMegaAvr));
    let defines = config.get_defines();
    assert_eq!(defines.get("ARDUINO_ARCH_MEGAAVR"), Some(&"1".to_string()));
}

#[test]
fn test_nano_every_board_config() {
    let config = BoardConfig::from_board_id("nano_every", &HashMap::new()).unwrap();
    assert_eq!(config.mcu, "atmega4809");
    assert_eq!(config.core, "arduino");
    assert_eq!(config.variant, "nona4809");
    assert_eq!(config.platform(), Some(fbuild_core::Platform::AtmelMegaAvr));
}

/// Validate that ALL megatinycore boards have the required framework-injected
/// defines in extra_flags. PlatformIO's builder script injects these at build
/// time, but fbuild must carry them in the board JSON since we don't run
/// PlatformIO's SCons scripts.
#[test]
fn test_megatinycore_boards_have_required_defines() {
    let db = get_board_db();
    let required = &[
        "MEGATINYCORE=",
        "MEGATINYCORE_MAJOR=",
        "MEGATINYCORE_MINOR=",
        "MEGATINYCORE_PATCH=",
        "MEGATINYCORE_RELEASED=",
        "CORE_ATTACH_ALL",
        "TWI_MORS",
        "CLOCK_SOURCE=",
    ];
    let mut failures = Vec::new();
    for (board_id, value) in db.iter() {
        let core = value
            .pointer("/build/core")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if core != "megatinycore" {
            continue;
        }
        let flags = value
            .pointer("/build/extra_flags")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        for &define in required {
            if !flags.contains(&format!("-D{define}")) {
                failures.push(format!("{board_id}: missing -D{define}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "megatinycore boards missing required framework defines:\n{}",
        failures.join("\n")
    );
}

/// Validate that ALL dxcore boards have the required framework-injected
/// defines in extra_flags.
#[test]
fn test_dxcore_boards_have_required_defines() {
    let db = get_board_db();
    let required = &[
        "DXCORE=",
        "DXCORE_MAJOR=",
        "DXCORE_MINOR=",
        "DXCORE_PATCH=",
        "DXCORE_RELEASED=",
        "CORE_ATTACH_ALL",
        "TWI_MORS_SINGLE",
        "MILLIS_USE_TIMER",
        "CLOCK_SOURCE=",
    ];
    let mut failures = Vec::new();
    for (board_id, value) in db.iter() {
        let core = value
            .pointer("/build/core")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if core != "dxcore" {
            continue;
        }
        let flags = value
            .pointer("/build/extra_flags")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        for &define in required {
            if !flags.contains(&format!("-D{define}")) {
                failures.push(format!("{board_id}: missing -D{define}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "dxcore boards missing required framework defines:\n{}",
        failures.join("\n")
    );
}

#[test]
fn test_uno_debug_tools_has_simavr() {
    let config = BoardConfig::from_board_id("uno", &HashMap::new()).unwrap();
    let tools = config
        .debug_tools
        .as_ref()
        .expect("uno should have debug tools");
    assert!(tools.contains_key("simavr"), "uno should have simavr");
    assert!(tools.contains_key("avr-stub"), "uno should have avr-stub");
    // simavr is not marked as onboard in the board JSON
    assert!(!tools["simavr"].onboard);
}

#[test]
fn test_emulators_filters_hardware_probes() {
    let config = BoardConfig::from_board_id("uno", &HashMap::new()).unwrap();
    let emus = config.emulators();
    // simavr is an emulator, avr-stub is not in EMULATOR_TOOL_NAMES
    assert!(emus.contains_key("simavr"), "simavr should be in emulators");
    assert!(
        !emus.contains_key("avr-stub"),
        "avr-stub is not an emulator"
    );
}

#[test]
fn test_has_emulator() {
    let config = BoardConfig::from_board_id("uno", &HashMap::new()).unwrap();
    assert!(config.has_emulator("simavr"));
    assert!(!config.has_emulator("qemu"));
    assert!(!config.has_emulator("avr-stub")); // not in EMULATOR_TOOL_NAMES
}

#[test]
fn test_debug_tools_round_trip_serde() {
    let config = BoardConfig::from_board_id("uno", &HashMap::new()).unwrap();
    let json = serde_json::to_string(&config).unwrap();
    let restored: BoardConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(config.debug_tools, restored.debug_tools);
}

#[test]
fn test_boards_txt_has_no_debug_tools() {
    let f = write_boards_txt(
        "\
uno.name=Arduino Uno
uno.build.mcu=atmega328p
uno.build.f_cpu=16000000L
",
    );
    let config = BoardConfig::from_boards_txt(f.path(), "uno", &HashMap::new()).unwrap();
    assert!(
        config.debug_tools.is_none(),
        "boards.txt should not have debug tools"
    );
}

#[test]
fn test_esp32c6_devkitc1_has_variant() {
    // Regression: esp32-c6-devkitc-1.json was missing build.variant,
    // causing fbuild to look for variants/esp32-c6-devkitc-1/ instead
    // of variants/esp32c6/, which broke compilation (pins_arduino.h
    // not found). See https://github.com/FastLED/fbuild/issues/46
    let config = BoardConfig::from_board_id("esp32-c6-devkitc-1", &HashMap::new()).unwrap();
    assert_eq!(config.mcu, "esp32c6");
    assert_eq!(config.core, "esp32");
    assert_eq!(
        config.variant, "esp32c6",
        "esp32-c6-devkitc-1 must have variant=esp32c6, not the board ID fallback"
    );
}

#[test]
fn test_esp32c6_alias_has_variant() {
    // The 'esp32c6' alias resolves to esp32-c6-devkitm-1 which has
    // the variant field. Verify both paths produce correct variant.
    let config = BoardConfig::from_board_id("esp32c6", &HashMap::new()).unwrap();
    assert_eq!(config.mcu, "esp32c6");
    assert_eq!(
        config.variant, "esp32c6",
        "esp32c6 alias must resolve to variant=esp32c6"
    );
}

#[test]
fn test_board_without_debug_section() {
    // Find a board that has no debug section (if any), or verify graceful handling
    // by checking that debug_tools is populated from the JSON when present
    let config = BoardConfig::from_board_id("esp32dev", &HashMap::new()).unwrap();
    // esp32dev has debug tools (hardware probes only, no emulators)
    if let Some(ref tools) = config.debug_tools {
        let emus = config.emulators();
        // esp32dev has no software emulators, only hardware probes
        assert!(
            emus.is_empty(),
            "esp32dev should have no emulators, got: {:?}",
            emus
        );
        // But it should still have hardware debug tools
        assert!(!tools.is_empty(), "esp32dev should have debug tools");
    }
}
