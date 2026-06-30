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
fn test_leonardo_board_has_vid_pid() {
    let config = BoardConfig::from_board_id("leonardo", &HashMap::new()).unwrap();
    assert_eq!(config.vid, Some("0x2341".to_string()));
    assert_eq!(config.pid, Some("0x8036".to_string()));
    let defines = config.get_defines();
    assert_eq!(defines.get("USB_VID"), Some(&"0x2341".to_string()));
    assert_eq!(defines.get("USB_PID"), Some(&"0x8036".to_string()));
}

#[test]
fn test_due_port_specific_vid_pid_rows() {
    for board_id in ["due", "sainSmartDue"] {
        let config = BoardConfig::from_board_id(board_id, &HashMap::new()).unwrap();
        assert_eq!(config.vid, Some("0x2341".to_string()));
        assert_eq!(config.pid, Some("0x003D".to_string()));
    }

    for board_id in ["dueUSB", "sainSmartDueUSB"] {
        let config = BoardConfig::from_board_id(board_id, &HashMap::new()).unwrap();
        assert_eq!(config.vid, Some("0x2341".to_string()));
        assert_eq!(config.pid, Some("0x003E".to_string()));
    }
}

#[test]
fn test_unexpected_maker_s3_usb_pid_rows_are_distinct() {
    let expected = [
        ("um_tinys3", "0X303A", "0x80D0"),
        ("um_feathers3", "0X303A", "0x80D6"),
        ("um_feathers3_neo", "0X303A", "0x81FB"),
    ];

    for (board_id, vid, pid) in expected {
        let config = BoardConfig::from_board_id(board_id, &HashMap::new()).unwrap();
        assert_eq!(config.vid, Some(vid.to_string()), "{board_id} VID");
        assert_eq!(config.pid, Some(pid.to_string()), "{board_id} PID");
    }
}

/// FastLED/fbuild#405: on ESP32-S2/S3 the Arduino framework's
/// `variants/<board>/pins_arduino.h` already defines `USB_VID`/`USB_PID`,
/// so injecting them again via `-D` produces a "USB_VID redefined" warning
/// at every TU that includes `pins_arduino.h` (149-156 per build observed
/// in CI). `get_defines()` must NOT emit them for ESP32-S2/S3 boards even
/// when the board JSON carries `vid`/`pid` fields.
#[test]
fn test_esp32s3_board_skips_usb_vid_pid_injection() {
    let config = BoardConfig::from_board_id("adafruit_feather_esp32s3", &HashMap::new()).unwrap();
    // Board JSON has vid/pid set (Adafruit Feather ESP32-S3 = 0x239A/0x811B).
    assert!(
        config.vid.is_some(),
        "esp32s3 board should have vid in JSON"
    );
    assert!(
        config.pid.is_some(),
        "esp32s3 board should have pid in JSON"
    );
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
/// only applies to the unconditional injection from the JSON `vid`/`pid`
/// fields.
#[test]
fn test_esp32s3_board_extra_flag_usb_vid_override_wins() {
    let mut config =
        BoardConfig::from_board_id("adafruit_feather_esp32s3", &HashMap::new()).unwrap();
    config.extra_flags = Some("-DUSB_VID=0xDEAD -DUSB_PID=0xBEEF".to_string());
    let defines = config.get_defines();
    assert_eq!(defines.get("USB_VID"), Some(&"0xDEAD".to_string()));
    assert_eq!(defines.get("USB_PID"), Some(&"0xBEEF".to_string()));
}
