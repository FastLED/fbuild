//! Best-effort read of the build's `usb_type` setting.
//!
//! Teensy supports several USB descriptors at link time
//! (`USB_SERIAL`, `USB_DUAL_SERIAL`, `USB_TRIPLE_SERIAL`, `USB_MIDI_SERIAL`,
//! `USB_RAWHID`, …). Only the variants that include a Serial endpoint will
//! produce bytes on the CDC ACM port that `fbuild monitor` attaches to.
//!
//! If the build wrote the chosen `usb_type` next to the firmware artifact
//! (e.g. `firmware.hex` + `firmware.usb_type`), we can read it back and warn
//! the user up-front instead of waiting for the "no bytes for 10s" probe to
//! fire.
//!
//! The reader is intentionally tolerant: any missing/unreadable file just
//! returns `None`. We never block a deploy because the metadata is absent.

use std::fs;
use std::path::Path;

/// Severity of the advisory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsbTypeAdvisory {
    /// `USB_SERIAL` / `USB_DUAL_SERIAL` / `USB_TRIPLE_SERIAL` —
    /// monitor attaches normally.
    Serial,
    /// `USB_MIDI_SERIAL` — monitor will read the Serial endpoint, but the user
    /// should know the device also exposes a MIDI endpoint that won't show up
    /// in `fbuild monitor`.
    MidiSerial,
    /// `USB_RAWHID` — no Serial endpoint at all. `fbuild monitor` will be
    /// permanently silent. This is a hard error in spirit but only advisory in
    /// practice — the user may have intended a HID-only sketch.
    RawHid,
    /// Some other variant that includes the word `SERIAL` — assume Serial
    /// works but call it out so the user has a breadcrumb.
    OtherSerial(String),
    /// Some other variant we don't recognise.
    Other(String),
}

impl UsbTypeAdvisory {
    /// Classify a raw `usb_type` string into one of the advisory variants.
    pub fn classify(raw: &str) -> Self {
        let trimmed = raw.trim();
        // Strip a leading `USB_` so callers can pass either form.
        let stripped = trimmed.strip_prefix("USB_").unwrap_or(trimmed);
        match stripped {
            "SERIAL" | "DUAL_SERIAL" | "TRIPLE_SERIAL" => UsbTypeAdvisory::Serial,
            "MIDI_SERIAL" | "MIDI_SERIAL_4X4" | "HID_SERIAL" => UsbTypeAdvisory::MidiSerial,
            "RAWHID" => UsbTypeAdvisory::RawHid,
            other if other.contains("SERIAL") => UsbTypeAdvisory::OtherSerial(other.to_string()),
            other => UsbTypeAdvisory::Other(other.to_string()),
        }
    }

    /// A short, user-facing advisory message, or `None` for the boring
    /// `Serial` case.
    pub fn advisory_message(&self) -> Option<String> {
        match self {
            UsbTypeAdvisory::Serial => None,
            UsbTypeAdvisory::MidiSerial => Some(
                "build's usb_type is MIDI_SERIAL — monitor is attached to the Serial endpoint, \
                 not MIDI"
                    .to_string(),
            ),
            UsbTypeAdvisory::RawHid => Some(
                "build's usb_type is RAWHID — there is no Serial endpoint; fbuild monitor will \
                 be permanently silent for this firmware"
                    .to_string(),
            ),
            UsbTypeAdvisory::OtherSerial(name) => Some(format!(
                "build's usb_type is {} — assuming Serial endpoint works",
                name
            )),
            UsbTypeAdvisory::Other(name) => Some(format!(
                "build's usb_type is {} — no Serial endpoint expected; monitor may be silent",
                name
            )),
        }
    }
}

/// Try to read `usb_type` from a sibling of `firmware_path`.
///
/// Looks for `<stem>.usb_type` next to the firmware first
/// (e.g. `firmware.usb_type`), then `usb_type.txt` in the firmware's parent
/// directory. Returns `None` if neither is present or readable.
pub fn read_usb_type_near(firmware_path: &Path) -> Option<String> {
    let parent = firmware_path.parent()?;
    if let Some(stem) = firmware_path.file_stem() {
        let sibling = parent.join(format!("{}.usb_type", stem.to_string_lossy()));
        if let Ok(s) = fs::read_to_string(&sibling) {
            let trimmed = s.trim().to_string();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }
    let fallback = parent.join("usb_type.txt");
    if let Ok(s) = fs::read_to_string(&fallback) {
        let trimmed = s.trim().to_string();
        if !trimmed.is_empty() {
            return Some(trimmed);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn classify_known_serial_variants() {
        assert_eq!(
            UsbTypeAdvisory::classify("USB_SERIAL"),
            UsbTypeAdvisory::Serial
        );
        assert_eq!(
            UsbTypeAdvisory::classify("DUAL_SERIAL"),
            UsbTypeAdvisory::Serial
        );
        assert_eq!(
            UsbTypeAdvisory::classify("USB_TRIPLE_SERIAL"),
            UsbTypeAdvisory::Serial
        );
    }

    #[test]
    fn classify_midi_serial() {
        assert_eq!(
            UsbTypeAdvisory::classify("USB_MIDI_SERIAL"),
            UsbTypeAdvisory::MidiSerial
        );
    }

    #[test]
    fn classify_rawhid() {
        assert_eq!(
            UsbTypeAdvisory::classify("USB_RAWHID"),
            UsbTypeAdvisory::RawHid
        );
    }

    #[test]
    fn classify_falls_through_to_other() {
        match UsbTypeAdvisory::classify("USB_KEYBOARDONLY") {
            UsbTypeAdvisory::Other(s) => assert!(s.contains("KEYBOARD")),
            other => panic!("expected Other, got {:?}", other),
        }
    }

    #[test]
    fn advisory_message_silent_for_serial() {
        assert!(UsbTypeAdvisory::Serial.advisory_message().is_none());
    }

    #[test]
    fn advisory_message_loud_for_rawhid() {
        let msg = UsbTypeAdvisory::RawHid.advisory_message().unwrap();
        assert!(msg.contains("RAWHID"));
        assert!(msg.contains("silent"));
    }

    #[test]
    fn read_usb_type_finds_sibling_file() {
        let dir = TempDir::new().unwrap();
        let fw = dir.path().join("firmware.hex");
        fs::write(&fw, b":00000001FF").unwrap();
        fs::write(dir.path().join("firmware.usb_type"), b"USB_MIDI_SERIAL\n").unwrap();
        assert_eq!(read_usb_type_near(&fw), Some("USB_MIDI_SERIAL".to_string()));
    }

    #[test]
    fn read_usb_type_falls_back_to_usb_type_txt() {
        let dir = TempDir::new().unwrap();
        let fw = dir.path().join("firmware.hex");
        fs::write(&fw, b":00000001FF").unwrap();
        fs::write(dir.path().join("usb_type.txt"), b"USB_SERIAL").unwrap();
        assert_eq!(read_usb_type_near(&fw), Some("USB_SERIAL".to_string()));
    }

    #[test]
    fn read_usb_type_returns_none_when_absent() {
        let dir = TempDir::new().unwrap();
        let fw = dir.path().join("firmware.hex");
        fs::write(&fw, b":00000001FF").unwrap();
        assert!(read_usb_type_near(&fw).is_none());
    }
}
