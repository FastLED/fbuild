//! USB VID/PID define-injection tests.
//!
//! Extracted from `tests.rs` to keep the main test module under the 1000-LOC
//! ceiling enforced by `.github/workflows/loc_gate.yml`.

use std::collections::HashMap;

use super::BoardConfig;

// Spell the backslash as a Unicode escape so the source-policy test can
// distinguish expected-value assertions from define construction code.
const ESCAPED_QUOTE: &str = "\u{5C}\"";

fn quoted(value: &str) -> String {
    format!("{ESCAPED_QUOTE}{value}{ESCAPED_QUOTE}")
}

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
fn test_bundled_board_usb_ids_are_not_local_defaults() {
    for board_id in ["leonardo", "due", "dueUSB", "um_feathers3"] {
        let config = BoardConfig::from_board_id(board_id, &HashMap::new()).unwrap();
        assert_eq!(
            config.vid, None,
            "{board_id} VID must come from FastLED/boards"
        );
        assert_eq!(
            config.pid, None,
            "{board_id} PID must come from FastLED/boards"
        );
    }
}

#[test]
fn test_registry_compile_identity_define_format() {
    assert_eq!(
        BoardConfig::formatted_registry_compile_identity(Some((0xfeed, 0xc0de))),
        Some(("0xFEED".to_string(), "0xC0DE".to_string()))
    );
    assert_eq!(BoardConfig::formatted_registry_compile_identity(None), None);
}

#[test]
#[ignore = "live FastLED/boards publication smoke test"]
fn live_registry_identity_drives_pico_compile_defines() {
    let temp = tempfile::tempdir().unwrap();
    let meta = temp.path().join("_meta.json");
    let profiles = temp.path().join("usb-profiles.json");
    assert!(fbuild_core::usb::profiles::populate_profiles_from_paths(
        &meta, &profiles
    ));

    let config = BoardConfig::from_board_id("rpipico", &HashMap::new()).unwrap();
    let defines = config.get_defines();
    assert_eq!(defines.get("USB_VID"), Some(&"0x2E8A".to_string()));
    assert_eq!(defines.get("USB_PID"), Some(&"0x000A".to_string()));
}

/// FastLED/fbuild#405: on ESP32-S2/S3 the Arduino framework's
/// `variants/<board>/pins_arduino.h` already defines `USB_VID`/`USB_PID`,
/// so injecting them again via `-D` produces a "USB_VID redefined" warning
/// at every TU that includes `pins_arduino.h` (149-156 per build observed
/// in CI). `get_defines()` must NOT emit them for ESP32-S2/S3 boards even
/// when an explicit project override supplies `vid`/`pid` fields.
#[test]
fn test_esp32s3_board_skips_usb_vid_pid_injection() {
    let mut config =
        BoardConfig::from_board_id("adafruit_feather_esp32s3", &HashMap::new()).unwrap();
    config.vid = Some("0x1234".to_string());
    config.pid = Some("0x5678".to_string());
    // get_defines() must NOT emit them — the framework variant header does.
    let defines = config.get_defines();
    assert!(
        !defines.contains_key("USB_VID"),
        "esp32s3 must not inject USB_VID — framework defines it"
    );
    assert!(
        !defines.contains_key("USB_PID"),
        "esp32s3 must not inject USB_PID — framework defines it"
    );
}

/// User override path: when a user sets `-DUSB_VID=...` via `build_flags`,
/// the `extra_flags` processing must still propagate it. The skip rule
/// only applies to the normal board compile identity path.
#[test]
fn test_esp32s3_board_extra_flag_usb_vid_override_wins() {
    let mut config =
        BoardConfig::from_board_id("adafruit_feather_esp32s3", &HashMap::new()).unwrap();
    config.extra_flags = Some("-DUSB_VID=0xDEAD -DUSB_PID=0xBEEF".to_string());
    let defines = config.get_defines();
    assert_eq!(defines.get("USB_VID"), Some(&"0xDEAD".to_string()));
    assert_eq!(defines.get("USB_PID"), Some(&"0xBEEF".to_string()));
}

/// PlatformIO parity (platform-atmelsam `arduino-common.py`): a board that
/// declares `build.usb_product` gets all four USB defines, with the string
/// defines quoted like `ARDUINO_BOARD` and embedded quotes stripped.
/// Synthetic IDs — test fixtures are exempt from the registry-only rule.
#[test]
fn test_usb_product_board_yields_all_four_defines() {
    let config = BoardConfig {
        mcu: "samd21g18a".to_string(),
        vid: Some("0x239A".to_string()),
        pid: Some("0x800B".to_string()),
        usb_product: Some("Adafruit Feather M0".to_string()),
        usb_manufacturer: Some("Adafruit".to_string()),
        ..Default::default()
    };
    let defines = config.get_defines();
    assert_eq!(defines.get("USB_VID"), Some(&"0x239A".to_string()));
    assert_eq!(defines.get("USB_PID"), Some(&"0x800B".to_string()));
    assert_eq!(
        defines.get("USB_PRODUCT"),
        Some(&quoted("Adafruit Feather M0"))
    );
    assert_eq!(defines.get("USB_MANUFACTURER"), Some(&quoted("Adafruit")));
}

/// Embedded double quotes are stripped from the string defines, matching
/// PlatformIO's `.replace('"', "")` sanitization.
#[test]
fn test_usb_string_defines_strip_embedded_quotes() {
    let config = BoardConfig {
        mcu: "samd21g18a".to_string(),
        usb_product: Some("Board \"Rev B\"".to_string()),
        usb_manufacturer: Some("Vendor \"Inc\"".to_string()),
        ..Default::default()
    };
    let defines = config.get_defines();
    assert_eq!(defines.get("USB_PRODUCT"), Some(&quoted("Board Rev B")));
    assert_eq!(defines.get("USB_MANUFACTURER"), Some(&quoted("Vendor Inc")));
}

/// The manufacturer string is gated on `usb_product` (PlatformIO defines
/// them in one block): every bundled board carries a top-level `vendor`,
/// and emitting USB_MANUFACTURER for all of them would churn the defines
/// of boards that never had USB flags — `uno` must stay untouched.
#[test]
fn test_boards_without_usb_product_gain_no_usb_string_defines() {
    let config = BoardConfig::from_board_id("uno", &HashMap::new()).unwrap();
    assert!(config.usb_product.is_none());
    let defines = config.get_defines();
    for key in ["USB_VID", "USB_PID", "USB_PRODUCT", "USB_MANUFACTURER"] {
        assert!(!defines.contains_key(key), "uno must not define {key}");
    }
    // Existing flags unaffected.
    assert_eq!(defines.get("F_CPU"), Some(&"16000000L".to_string()));
    assert_eq!(defines.get("ARDUINO_ARCH_AVR"), Some(&"1".to_string()));
}

/// PlatformIO-format project-local board manifests carry USB identities as
/// `build.hwids: [[vid, pid], ...]` plus `build.usb_product` / top-level
/// `vendor`. `hwids[0]` is the compile identity (arduino-common.py). This
/// path only applies to user-supplied manifests: bundled snapshots are
/// hwids-free by the registry-boundary guard.
#[test]
fn test_project_local_pio_manifest_hwids_drive_usb_defines() {
    let temp = tempfile::tempdir().unwrap();
    let boards_dir = temp.path().join("boards");
    std::fs::create_dir_all(&boards_dir).unwrap();
    std::fs::write(
        boards_dir.join("synthetic_usb_board.json"),
        r#"{
          "build": {
            "core": "adafruit",
            "extra_flags": "-DARDUINO_SAMD_ZERO",
            "f_cpu": "48000000L",
            "hwids": [["0x1111", "0x8222"], ["0x1111", "0x0222"]],
            "mcu": "samd21g18a",
            "usb_product": "Synthetic Board",
            "variant": "standard"
          },
          "name": "Synthetic USB Board",
          "upload": {"maximum_ram_size": 32768, "maximum_size": 262144},
          "vendor": "Synthetic Vendor"
        }"#,
    )
    .unwrap();

    let config = BoardConfig::from_board_id_in_project(
        "synthetic_usb_board",
        &HashMap::new(),
        Some(temp.path()),
    )
    .unwrap();
    assert_eq!(config.vid.as_deref(), Some("0x1111"));
    assert_eq!(config.pid.as_deref(), Some("0x8222"));
    assert_eq!(config.usb_product.as_deref(), Some("Synthetic Board"));
    assert_eq!(config.usb_manufacturer.as_deref(), Some("Synthetic Vendor"));

    let defines = config.get_defines();
    assert_eq!(defines.get("USB_VID"), Some(&"0x1111".to_string()));
    assert_eq!(defines.get("USB_PID"), Some(&"0x8222".to_string()));
    assert_eq!(
        defines.get("USB_PRODUCT"),
        Some(&quoted("Synthetic Board"))
    );
    assert_eq!(
        defines.get("USB_MANUFACTURER"),
        Some(&quoted("Synthetic Vendor"))
    );
}

/// Explicit `build.vid`/`build.pid` keys win over `build.hwids[0]` when a
/// manifest carries both.
#[test]
fn test_explicit_vid_pid_keys_win_over_hwids() {
    let temp = tempfile::tempdir().unwrap();
    let boards_dir = temp.path().join("boards");
    std::fs::create_dir_all(&boards_dir).unwrap();
    std::fs::write(
        boards_dir.join("synthetic_explicit_board.json"),
        r#"{
          "build": {
            "hwids": [["0x1111", "0x8222"]],
            "mcu": "samd21g18a",
            "pid": "0x0444",
            "vid": "0x0333"
          },
          "name": "Synthetic Explicit Board"
        }"#,
    )
    .unwrap();

    let config = BoardConfig::from_board_id_in_project(
        "synthetic_explicit_board",
        &HashMap::new(),
        Some(temp.path()),
    )
    .unwrap();
    assert_eq!(config.vid.as_deref(), Some("0x0333"));
    assert_eq!(config.pid.as_deref(), Some("0x0444"));
}

/// Bundled SAMD boards regained their PlatformIO `usb_product` string (it is
/// not a USB identity, so it lives in the snapshot), giving USB_PRODUCT /
/// USB_MANUFACTURER parity. USB_VID/USB_PID still come exclusively from the
/// FastLED/boards registry at runtime.
#[test]
fn test_bundled_samd_boards_carry_usb_product_strings() {
    for (board_id, product) in [
        ("adafruit_feather_m0", "Adafruit Feather M0"),
        ("zeroUSB", "Arduino Zero"),
        ("adafruit_qt_py_m0", "QT Py M0"),
        ("adafruit_feather_m4", "Adafruit Feather M4"),
        ("adafruit_grandcentral_m4", "Adafruit Grand Central M4"),
    ] {
        let config = BoardConfig::from_board_id(board_id, &HashMap::new()).unwrap();
        assert_eq!(
            config.usb_product.as_deref(),
            Some(product),
            "{board_id} usb_product"
        );
        assert!(config.usb_manufacturer.is_some(), "{board_id} vendor");
        let defines = config.get_defines();
        assert!(
            defines.contains_key("USB_PRODUCT"),
            "{board_id} USB_PRODUCT"
        );
    }
}
