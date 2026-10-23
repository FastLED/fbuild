//! Bootloader entry for ESP32-S2/S3 boards running a TinyUSB CDC app.
//!
//! Arduino firmware built with `ARDUINO_USB_MODE=0` +
//! `ARDUINO_USB_CDC_ON_BOOT=1` drives the USB-OTG peripheral with
//! TinyUSB. That CDC port is application code, so esptool's DTR/RTS
//! reset sequence never reaches the ROM ("No serial data received").
//! The Arduino core reboots into ROM download mode on the standard
//! 1200-bps touch; the chip then re-enumerates (on the S3, as the
//! USB-Serial-JTAG peripheral, whose serial is the colon-separated MAC
//! instead of TinyUSB's bare hex MAC), possibly at a different path.

use std::time::{Duration, Instant};

use fbuild_core::{FbuildError, Result};
use fbuild_serial::boards::BoardFamily;
use serialport::SerialPortType;

/// How long the chip gets to drop the TinyUSB port and re-enumerate
/// in ROM download mode after the 1200-bps touch.
const REENUMERATION_DEADLINE: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The USB facts of one enumerated serial port that matter here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PortSnapshot {
    pub name: String,
    pub serial: Option<String>,
    pub product: Option<String>,
}

/// Normalize a USB serial number so TinyUSB's `D83BDA4164C0` and the
/// USB-Serial-JTAG peripheral's `D8:3B:DA:41:64:C0` compare equal.
pub(super) fn normalize_usb_serial(serial: &str) -> String {
    serial
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// The ROM / HWCDC USB-Serial-JTAG peripheral — esptool resets it itself.
fn is_usb_serial_jtag_product(product: &str) -> bool {
    product.to_ascii_lowercase().contains("jtag")
}

/// `true` when `port` is an ESP32-S2/S3 TinyUSB application CDC port
/// that must be touched at 1200 bps before esptool can connect.
///
/// Requires an Espressif native-USB identity (not a USB-UART bridge),
/// a product string that is not the USB-Serial-JTAG peripheral, and a
/// MAC-shaped serial number (the S2 ROM's own CDC reports serial `0`,
/// and is already in download mode).
pub(super) fn needs_tinyusb_bootloader_touch(
    chip: &str,
    family: Option<BoardFamily>,
    port: &PortSnapshot,
) -> bool {
    if !matches!(chip.to_ascii_lowercase().as_str(), "esp32s2" | "esp32s3") {
        return false;
    }
    if family != Some(BoardFamily::Esp32NativeUsbCdc) {
        return false;
    }
    let Some(product) = port.product.as_deref() else {
        return false;
    };
    if is_usb_serial_jtag_product(product) {
        return false;
    }
    port.serial.as_deref().is_some_and(|s| {
        let n = normalize_usb_serial(s);
        n.len() == 12 && n.chars().all(|c| c.is_ascii_hexdigit())
    })
}

/// Pick the port the chip re-enumerated as after the touch: same
/// normalized serial (or same path) but no longer the TinyUSB app.
pub(super) fn pick_reenumerated_port(
    ports: &[PortSnapshot],
    original: &PortSnapshot,
) -> Option<String> {
    let want = original.serial.as_deref().map(normalize_usb_serial);
    ports
        .iter()
        .filter(|p| p.product != original.product)
        .filter(|p| p.product.is_some())
        .find(|p| p.serial.as_deref().map(normalize_usb_serial) == want || p.name == original.name)
        .map(|p| p.name.clone())
}

fn snapshot_ports() -> Vec<(PortSnapshot, Option<BoardFamily>)> {
    let ports = match fbuild_serial::ports::available_ports() {
        Ok(ports) => ports,
        Err(e) => {
            tracing::warn!("port enumeration failed: {}", e);
            return Vec::new();
        }
    };
    ports
        .into_iter()
        .filter(|p| !p.health.is_known_unhealthy())
        .map(|p| match p.info.port_type {
            SerialPortType::UsbPort(usb) => (
                PortSnapshot {
                    name: p.info.port_name,
                    serial: usb.serial_number,
                    product: usb.product,
                },
                fbuild_serial::boards::family_for_vid_pid(usb.vid, usb.pid),
            ),
            _ => (
                PortSnapshot {
                    name: p.info.port_name,
                    serial: None,
                    product: None,
                },
                None,
            ),
        })
        .collect()
}

fn touch_1200bps(port: &str) -> Result<()> {
    match serialport::new(port, 1200)
        .timeout(Duration::from_secs(2))
        .open()
    {
        // Arduino-ESP32's USBCDC reboots into download mode when the
        // line coding is 1200 baud and DTR drops; dropping the handle
        // after deasserting DTR completes the touch.
        Ok(mut serial) => {
            let _ = serial.write_data_terminal_ready(false);
            Ok(())
        }
        Err(e) if e.kind() == serialport::ErrorKind::NoDevice => Ok(()),
        Err(e) => Err(FbuildError::DeployFailed(format!(
            "failed to open {port} at 1200 bps for TinyUSB bootloader entry: {e}"
        ))),
    }
}

/// If `port` is a TinyUSB CDC app port on an S2/S3, touch it at
/// 1200 bps, wait (bounded) for the ROM download-mode re-enumeration
/// and return the port to flash. Every other port is returned as-is.
pub(super) fn enter_download_mode_blocking(chip: &str, port: &str) -> Result<String> {
    let Some(original) = snapshot_ports()
        .into_iter()
        .find(|(p, family)| p.name == port && needs_tinyusb_bootloader_touch(chip, *family, p))
        .map(|(p, _)| p)
    else {
        return Ok(port.to_string());
    };

    tracing::info!(
        port,
        product = original.product.as_deref().unwrap_or(""),
        "{chip} is running a TinyUSB CDC app; 1200-bps touch to enter ROM download mode"
    );
    touch_1200bps(port)?;

    let deadline = Instant::now() + REENUMERATION_DEADLINE;
    loop {
        let ports: Vec<PortSnapshot> = snapshot_ports().into_iter().map(|(p, _)| p).collect();
        if let Some(found) = pick_reenumerated_port(&ports, &original) {
            tracing::info!(port, resolved = %found, "{chip} re-enumerated in download mode");
            return Ok(found);
        }
        if Instant::now() >= deadline {
            return Err(FbuildError::DeployFailed(format!(
                "{chip} on {port} (TinyUSB CDC, serial {}) did not re-enumerate in ROM download \
                 mode within {}s of the 1200-bps touch; hold BOOT and tap RESET, then retry",
                original.serial.as_deref().unwrap_or("?"),
                REENUMERATION_DEADLINE.as_secs()
            )));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(name: &str, serial: Option<&str>, product: Option<&str>) -> PortSnapshot {
        PortSnapshot {
            name: name.to_string(),
            serial: serial.map(str::to_string),
            product: product.map(str::to_string),
        }
    }

    const NATIVE: Option<BoardFamily> = Some(BoardFamily::Esp32NativeUsbCdc);

    #[test]
    fn serial_normalization_ignores_separators_and_case() {
        assert_eq!(normalize_usb_serial("D8:3B:DA:41:64:C0"), "D83BDA4164C0");
        assert_eq!(normalize_usb_serial("d83bda4164c0"), "D83BDA4164C0");
        assert_eq!(normalize_usb_serial("D8-3B-DA-41-64-C0"), "D83BDA4164C0");
    }

    #[test]
    fn tinyusb_app_port_on_s3_needs_touch() {
        let p = snap(
            "/dev/ttyACM0",
            Some("D83BDA4164C0"),
            Some("ESP32_S3_DEVKITC_1"),
        );
        assert!(needs_tinyusb_bootloader_touch("esp32s3", NATIVE, &p));
        assert!(needs_tinyusb_bootloader_touch("esp32s2", NATIVE, &p));
    }

    #[test]
    fn usb_serial_jtag_port_does_not_need_touch() {
        let p = snap(
            "/dev/ttyACM0",
            Some("D8:3B:DA:41:64:C0"),
            Some("USB JTAG/serial debug unit"),
        );
        assert!(!needs_tinyusb_bootloader_touch("esp32s3", NATIVE, &p));
    }

    #[test]
    fn bridges_other_chips_and_rom_cdc_do_not_need_touch() {
        let app = snap(
            "/dev/ttyACM0",
            Some("D83BDA4164C0"),
            Some("ESP32_S3_DEVKITC_1"),
        );
        let bridge = Some(BoardFamily::Esp32ExternalUart);
        assert!(!needs_tinyusb_bootloader_touch("esp32s3", bridge, &app));
        assert!(!needs_tinyusb_bootloader_touch("esp32s3", None, &app));
        assert!(!needs_tinyusb_bootloader_touch("esp32c6", NATIVE, &app));
        assert!(!needs_tinyusb_bootloader_touch("esp32", NATIVE, &app));
        // S2 ROM download-mode CDC reports serial "0".
        let rom = snap("/dev/ttyACM0", Some("0"), Some("ESP32-S2"));
        assert!(!needs_tinyusb_bootloader_touch("esp32s2", NATIVE, &rom));
        let no_product = snap("/dev/ttyACM0", Some("D83BDA4164C0"), None);
        assert!(!needs_tinyusb_bootloader_touch(
            "esp32s3",
            NATIVE,
            &no_product
        ));
    }

    #[test]
    fn reenumerated_port_resolved_by_normalized_serial_at_new_path() {
        let original = snap(
            "/dev/ttyACM0",
            Some("D83BDA4164C0"),
            Some("ESP32_S3_DEVKITC_1"),
        );
        let ports = vec![
            snap("/dev/ttyUSB0", Some("01A2B3C4"), Some("CP2104 USB to UART")),
            snap(
                "/dev/ttyACM1",
                Some("8C:BF:EA:CF:87:B4"),
                Some("USB JTAG/serial debug unit"),
            ),
            snap(
                "/dev/ttyACM2",
                Some("D8:3B:DA:41:64:C0"),
                Some("USB JTAG/serial debug unit"),
            ),
        ];
        assert_eq!(
            pick_reenumerated_port(&ports, &original).as_deref(),
            Some("/dev/ttyACM2")
        );
    }

    #[test]
    fn still_running_app_or_absent_is_not_reenumerated() {
        let original = snap(
            "/dev/ttyACM0",
            Some("D83BDA4164C0"),
            Some("ESP32_S3_DEVKITC_1"),
        );
        assert_eq!(
            pick_reenumerated_port(std::slice::from_ref(&original), &original),
            None
        );
        assert_eq!(pick_reenumerated_port(&[], &original), None);
        let other = snap(
            "/dev/ttyACM1",
            Some("8C:BF:EA:CF:87:B4"),
            Some("USB JTAG/serial debug unit"),
        );
        assert_eq!(pick_reenumerated_port(&[other], &original), None);
    }

    #[test]
    fn same_path_with_rom_identity_counts_as_reenumerated() {
        let original = snap("/dev/ttyACM0", Some("D83BDA4164C0"), Some("ESP32-S2 app"));
        let rom = snap("/dev/ttyACM0", Some("0"), Some("ESP32-S2"));
        assert_eq!(
            pick_reenumerated_port(&[rom], &original).as_deref(),
            Some("/dev/ttyACM0")
        );
    }
}
