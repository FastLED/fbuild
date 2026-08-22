//! Selected macOS device mechanics: device-node-naming kernel-driver
//! classification (FastLED/fbuild#895) and the portable `serialport`
//! enumeration delegate behind [`crate::platform::device`].
//!
//! macOS device-node naming is set per-driver and is the canonical
//! signal here — no IOKit query is needed to distinguish the cases this
//! module covers:
//!
//! - `IOUSBHostFamily`'s CDC-ACM stack publishes `/dev/cu.usbmodem*`
//!   and `/dev/tty.usbmodem*` when the device exposes the CDC class.
//! - Vendor drivers publish their own prefixes (`cu.usbserial-*`,
//!   `cu.SLAB_USBtoUART*`, `cu.wchusbserial*`, `cu.PL2303-*`).

use std::io;

use crate::platform::device::{KernelDriverClass, SerialPortFacts, facts_from_port_info};

pub(crate) fn available_serial_ports() -> io::Result<Vec<SerialPortFacts>> {
    let ports = serialport::available_ports()?;
    Ok(ports.into_iter().map(facts_from_port_info).collect())
}

pub(crate) fn detect_serial_kernel_driver(port_name: &str) -> Option<KernelDriverClass> {
    classify_macos_devnode(port_name)
}

pub(crate) fn live_sysfs_usb_root() -> Option<crate::path::NormalizedPath> {
    None
}

pub(crate) fn mount_block_devices(_device_paths: &[&str]) {
    // No fbuild-supported auto-mount mechanic on macOS: macOS auto-mounts
    // USB mass-storage volumes itself.
}

/// Classify a macOS serial devnode from its device-node name.
///
/// Any name we don't recognize returns `None` so the caller falls back
/// to its existing default — same safety contract as the rest of the
/// device facade.
pub(crate) fn classify_macos_devnode(port_name: &str) -> Option<KernelDriverClass> {
    // Strip `/dev/` prefix if present, then strip the cu./tty.
    // disambiguation prefix.
    let bare = port_name.strip_prefix("/dev/").unwrap_or(port_name);
    let suffix = bare
        .strip_prefix("cu.")
        .or_else(|| bare.strip_prefix("tty."))
        .unwrap_or(bare);

    if suffix.starts_with("usbmodem") {
        return Some(KernelDriverClass::CdcAcm);
    }
    if suffix.starts_with("usbserial-")
        || suffix.starts_with("usbserial.")
        || suffix.starts_with("SLAB_USBtoUART")
        || suffix.starts_with("wchusbserial")
        || suffix.starts_with("PL2303")
    {
        return Some(KernelDriverClass::UsbSerialBridge);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_usbmodem_is_cdc() {
        assert_eq!(
            classify_macos_devnode("/dev/cu.usbmodem14101"),
            Some(KernelDriverClass::CdcAcm)
        );
        assert_eq!(
            classify_macos_devnode("/dev/tty.usbmodem14101"),
            Some(KernelDriverClass::CdcAcm)
        );
    }

    #[test]
    fn macos_ftdi_usbserial_is_bridge() {
        assert_eq!(
            classify_macos_devnode("/dev/cu.usbserial-A1234567"),
            Some(KernelDriverClass::UsbSerialBridge)
        );
        assert_eq!(
            classify_macos_devnode("/dev/tty.usbserial-FTDI"),
            Some(KernelDriverClass::UsbSerialBridge)
        );
    }

    #[test]
    fn macos_slab_cp210x_is_bridge() {
        assert_eq!(
            classify_macos_devnode("/dev/cu.SLAB_USBtoUART"),
            Some(KernelDriverClass::UsbSerialBridge)
        );
    }

    #[test]
    fn macos_wch_ch340_is_bridge() {
        assert_eq!(
            classify_macos_devnode("/dev/cu.wchusbserial1410"),
            Some(KernelDriverClass::UsbSerialBridge)
        );
    }

    #[test]
    fn macos_pl2303_is_bridge() {
        assert_eq!(
            classify_macos_devnode("/dev/cu.PL2303-XYZ"),
            Some(KernelDriverClass::UsbSerialBridge)
        );
    }

    #[test]
    fn macos_bare_name_without_dev_prefix() {
        // Caller passed in a bare name — should still work.
        assert_eq!(
            classify_macos_devnode("cu.usbmodem1101"),
            Some(KernelDriverClass::CdcAcm)
        );
    }

    #[test]
    fn macos_unrelated_returns_none() {
        // /dev/cu.Bluetooth-Incoming-Port shouldn't be classified as
        // either CDC or bridge — it's not USB.
        assert_eq!(classify_macos_devnode("/dev/cu.Bluetooth-Incoming-Port"), None);
        // Stray random name returns None too.
        assert_eq!(classify_macos_devnode("/dev/cu.random-thing"), None);
    }
}
