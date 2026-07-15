//! USB VID/PID define-injection tests.
//!
//! Extracted from `tests.rs` to keep the main test module under the 1000-LOC
//! ceiling enforced by `.github/workflows/loc_gate.yml`.

use std::collections::HashMap;

use super::BoardConfig;

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
        assert_eq!(config.vid, None, "{board_id} VID must come from FastLED/boards");
        assert_eq!(config.pid, None, "{board_id} PID must come from FastLED/boards");
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
    let mut config = BoardConfig::from_board_id("adafruit_feather_esp32s3", &HashMap::new()).unwrap();
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
