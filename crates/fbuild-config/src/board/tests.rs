//! Unit tests for the board module.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use tempfile::NamedTempFile;

use super::loaders::parse_boards_txt;
use super::BoardConfig;

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
fn test_from_board_id_arduino_uno_q() {
    let config = BoardConfig::from_board_id("arduino_uno_q", &HashMap::new()).unwrap();
    assert_eq!(config.name, "Arduino UNO Q");
    assert_eq!(config.mcu, "stm32u585zit6q");
    assert_eq!(config.variant, "STM32U5xx/U575Z(G-I)TxQ_U585ZITxQ");
    assert_eq!(
        config.variant_h.as_deref(),
        Some("variant_NUCLEO_U575ZI_Q.h")
    );
    assert_eq!(config.board, "UNO_Q");
    let defines = config.get_defines();
    assert_eq!(defines.get("ARDUINO_UNO_Q"), Some(&"1".to_string()));
    assert_eq!(
        defines.get("ARDUINO_NUCLEO_U575ZI_Q"),
        Some(&"1".to_string())
    );
    assert!(!defines.contains_key("ARDUINO_ARDUINO_UNO_Q"));
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
fn test_from_board_id_or_default_uses_primary_when_known() {
    let config = BoardConfig::from_board_id_or_default("mega", "uno", &HashMap::new(), None);
    assert_eq!(config.mcu, "atmega2560");
}

#[test]
fn test_from_board_id_or_default_falls_back_when_unknown() {
    let config =
        BoardConfig::from_board_id_or_default("nonexistent_board", "uno", &HashMap::new(), None);
    assert_eq!(config.mcu, "atmega328p");
}

#[test]
fn test_from_board_id_or_default_carries_overrides_into_fallback() {
    let mut overrides = HashMap::new();
    overrides.insert("f_cpu".to_string(), "8000000L".to_string());
    let config =
        BoardConfig::from_board_id_or_default("nonexistent_board", "uno", &overrides, None);
    assert_eq!(config.f_cpu, "8000000L");
    assert_eq!(config.mcu, "atmega328p");
}

#[test]
fn test_from_board_id_with_override_fallback_known() {
    let board = BoardConfig::from_board_id_with_override_fallback("uno", &HashMap::new(), None);
    assert_eq!(board.unwrap().mcu, "atmega328p");
}

#[test]
fn test_from_board_id_with_override_fallback_unknown_returns_none() {
    let board = BoardConfig::from_board_id_with_override_fallback(
        "nonexistent_board",
        &HashMap::new(),
        None,
    );
    assert!(board.is_none());
}

#[test]
fn test_from_board_id_with_override_fallback_applies_overrides() {
    let mut overrides = HashMap::new();
    overrides.insert("f_cpu".to_string(), "8000000L".to_string());
    let board = BoardConfig::from_board_id_with_override_fallback("uno", &overrides, None);
    assert_eq!(board.unwrap().f_cpu, "8000000L");
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
fn test_non_esp32_monitor_filters_default_absent() {
    let config = BoardConfig::from_board_id("uno", &HashMap::new()).unwrap();
    assert_eq!(config.monitor_filters, None);
    assert_eq!(config.monitor_filters_ini_value(), None);
}

#[test]
fn test_custom_monitor_filters_emit_in_platformio_format() {
    let mut overrides = HashMap::new();
    overrides.insert(
        "monitor_filters".to_string(),
        "default, time, log2file".to_string(),
    );

    let config = BoardConfig::from_board_id("uno", &overrides).unwrap();

    assert_eq!(
        config.monitor_filters,
        Some(vec![
            "default".to_string(),
            "time".to_string(),
            "log2file".to_string()
        ])
    );
    assert_eq!(
        config.monitor_filters_ini_value(),
        Some("default, time, log2file".to_string())
    );
}

#[test]
fn test_empty_monitor_filters_suppresses_emit() {
    let mut overrides = HashMap::new();
    overrides.insert("monitor_filters".to_string(), "[]".to_string());

    let config = BoardConfig::from_board_id("esp32dev", &overrides).unwrap();

    assert_eq!(config.monitor_filters, Some(Vec::new()));
    assert_eq!(config.monitor_filters_ini_value(), None);
}

#[test]
fn test_check_tool_default_absent() {
    let config = BoardConfig::from_board_id("esp32dev", &HashMap::new()).unwrap();
    assert_eq!(config.check_tool, None);
    assert_eq!(config.check_tool_ini_value(), None);
}

#[test]
fn test_check_tool_override_emits_static_analysis_tool() {
    let mut overrides = HashMap::new();
    overrides.insert("check_tool".to_string(), "clangtidy".to_string());

    let config = BoardConfig::from_board_id("uno", &overrides).unwrap();

    assert_eq!(config.check_tool.as_deref(), Some("clangtidy"));
    assert_eq!(config.check_tool_ini_value(), Some("clangtidy"));
}

#[test]
fn test_empty_check_tool_does_not_emit() {
    let mut overrides = HashMap::new();
    overrides.insert("check_tool".to_string(), "   ".to_string());

    let config = BoardConfig::from_board_id("uno", &overrides).unwrap();

    assert_eq!(config.check_tool.as_deref(), Some("   "));
    assert_eq!(config.check_tool_ini_value(), None);
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
