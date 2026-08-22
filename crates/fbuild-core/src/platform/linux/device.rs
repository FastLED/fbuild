//! Selected Linux device mechanics: sysfs-backed kernel-driver
//! classification (FastLED/fbuild#895) and the portable `serialport`
//! enumeration delegate behind [`crate::platform::device`].
//!
//! No library bridges "serial port name → kernel driver class" without
//! an OS-specific linking step, so this module reads the authoritative
//! source directly: a pure `std::fs::read_link` on
//! `/sys/class/tty/<name>/device/driver` — no libudev dependency.

use std::io;
use std::path::Path;

use crate::path::NormalizedPath;
use crate::platform::device::{KernelDriverClass, SerialPortFacts, facts_from_port_info};

/// Canonical sysfs USB topology root.
pub(crate) const SYSFS_USB_ROOT: &str = "/sys/bus/usb/devices";

pub(crate) fn available_serial_ports() -> io::Result<Vec<SerialPortFacts>> {
    let ports = serialport::available_ports()?;
    Ok(ports.into_iter().map(facts_from_port_info).collect())
}

pub(crate) fn detect_serial_kernel_driver(port_name: &str) -> Option<KernelDriverClass> {
    detect_with_sysfs_root(port_name, Path::new("/sys"))
}

pub(crate) fn live_sysfs_usb_root() -> Option<NormalizedPath> {
    let root = Path::new(SYSFS_USB_ROOT);
    if !root.is_dir() {
        return None;
    }
    Some(NormalizedPath::from(SYSFS_USB_ROOT))
}

pub(crate) fn mount_block_devices(device_paths: &[&str]) {
    for device in device_paths {
        let args = ["udisksctl", "mount", "--block-device", device];
        match crate::subprocess::run_command_blocking(
            &args,
            None,
            None,
            Some(std::time::Duration::from_secs(5)),
        ) {
            Ok(output) if output.success() => {
                tracing::debug!(device, "mounted RP-series ROM volume with udisksctl");
            }
            Ok(output) => {
                tracing::debug!(
                    device,
                    stderr = output.stderr.trim(),
                    "udisksctl could not mount RP-series ROM volume"
                );
            }
            Err(error) => {
                tracing::debug!(device, error = %error, "RP-series ROM auto-mount unavailable");
            }
        }
    }
}

/// Linux implementation, factored on `sysfs_root` so unit tests can
/// point at a temp dir holding a fake sysfs.
pub(crate) fn detect_with_sysfs_root(
    port_name: &str,
    sysfs_root: &Path,
) -> Option<KernelDriverClass> {
    let bare = port_name_stem(port_name)?;

    // 1. Authoritative path: read the driver symlink.
    //    /sys/class/tty/<name>/device/driver  -> a driver dir
    //    e.g. -> .../bus/usb-serial/drivers/cdc_acm
    //          -> .../bus/usb-serial/drivers/ftdi_sio
    //          -> .../bus/usb-serial/drivers/cp210x
    if let Some(driver_name) = read_driver_symlink_name(sysfs_root, bare) {
        return Some(classify_driver(&driver_name));
    }

    // 2. Fallback: the kernel's device-node naming convention.
    //    `ttyACM*` is created by `cdc_acm.ko`; `ttyUSB*` by
    //    `usbserial.ko`. If sysfs isn't readable for some reason
    //    (container, permissions), the name is still a strong signal
    //    because the kernel picks the prefix based on which driver
    //    claimed the device.
    classify_by_devnode_name(bare)
}

fn read_driver_symlink_name(sysfs_root: &Path, port_stem: &str) -> Option<String> {
    let driver_link = sysfs_root
        .join("class")
        .join("tty")
        .join(port_stem)
        .join("device")
        .join("driver");
    let target = std::fs::read_link(&driver_link).ok()?;
    target
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
}

/// Strip `/dev/` (or `/devices/` in some odd configurations) and return
/// the bare port name, e.g. `ttyACM0`.
pub(crate) fn port_name_stem(port_name: &str) -> Option<&str> {
    if let Some(stem) = port_name.strip_prefix("/dev/") {
        return Some(stem);
    }
    // Already a bare name (passed from a test or a sysfs walker).
    if !port_name.contains('/') {
        return Some(port_name);
    }
    // Pull the last path segment as a last resort.
    port_name.rsplit('/').next()
}

/// Classify a driver name pulled from the sysfs symlink.
///
/// `cdc_acm` is the unambiguous CDC-ACM driver. Anything else defaults
/// to bridge: the kernel only invokes `cdc_acm` for actual CDC class —
/// every other usb-serial driver is a chip-specific bridge by
/// construction.
pub(crate) fn classify_driver(driver_name: &str) -> KernelDriverClass {
    match driver_name {
        "cdc_acm" => KernelDriverClass::CdcAcm,
        _ => KernelDriverClass::UsbSerialBridge,
    }
}

/// Fall back to device-node naming when sysfs isn't readable.
pub(crate) fn classify_by_devnode_name(port_stem: &str) -> Option<KernelDriverClass> {
    if port_stem.starts_with("ttyACM") {
        return Some(KernelDriverClass::CdcAcm);
    }
    if port_stem.starts_with("ttyUSB") {
        return Some(KernelDriverClass::UsbSerialBridge);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // Build a fake `/sys/class/tty/<port>/device/driver` symlink
    // pointing at a fake driver dir under a tmp root, then ask
    // detect_with_sysfs_root to classify it.
    fn build_fake_sysfs_tree(
        sysfs_root: &std::path::Path,
        port_stem: &str,
        driver_name: &str,
    ) {
        let device_dir = sysfs_root
            .join("class")
            .join("tty")
            .join(port_stem)
            .join("device");
        std::fs::create_dir_all(&device_dir).unwrap();
        // The driver target dir must exist (the actual sysfs has it;
        // read_link only follows the symlink, but having a real target
        // dir matches the production shape).
        let driver_dir = sysfs_root
            .join("bus")
            .join("usb-serial")
            .join("drivers")
            .join(driver_name);
        std::fs::create_dir_all(&driver_dir).unwrap();
        crate::platform::fs::symlink_dir(&driver_dir, &device_dir.join("driver")).unwrap();
    }

    #[test]
    fn linux_sysfs_cdc_acm_driver_is_cdc() {
        let tmp = tempdir().unwrap();
        build_fake_sysfs_tree(tmp.path(), "ttyACM0", "cdc_acm");
        assert_eq!(
            detect_with_sysfs_root("/dev/ttyACM0", tmp.path()),
            Some(KernelDriverClass::CdcAcm)
        );
    }

    #[test]
    fn linux_sysfs_ftdi_driver_is_bridge() {
        let tmp = tempdir().unwrap();
        build_fake_sysfs_tree(tmp.path(), "ttyUSB0", "ftdi_sio");
        assert_eq!(
            detect_with_sysfs_root("/dev/ttyUSB0", tmp.path()),
            Some(KernelDriverClass::UsbSerialBridge)
        );
    }

    #[test]
    fn linux_sysfs_cp210x_driver_is_bridge() {
        let tmp = tempdir().unwrap();
        build_fake_sysfs_tree(tmp.path(), "ttyUSB1", "cp210x");
        assert_eq!(
            detect_with_sysfs_root("/dev/ttyUSB1", tmp.path()),
            Some(KernelDriverClass::UsbSerialBridge)
        );
    }

    #[test]
    fn linux_sysfs_ch341_driver_is_bridge() {
        let tmp = tempdir().unwrap();
        build_fake_sysfs_tree(tmp.path(), "ttyUSB2", "ch341");
        assert_eq!(
            detect_with_sysfs_root("/dev/ttyUSB2", tmp.path()),
            Some(KernelDriverClass::UsbSerialBridge)
        );
    }

    #[test]
    fn linux_devnode_name_acm_is_cdc() {
        // No sysfs entry exists at all → fall back to devnode name.
        // ttyACM* is created only by cdc_acm so this is reliable.
        let tmp = tempdir().unwrap();
        assert_eq!(
            detect_with_sysfs_root("/dev/ttyACM7", tmp.path()),
            Some(KernelDriverClass::CdcAcm)
        );
    }

    #[test]
    fn linux_devnode_name_usb_is_bridge() {
        let tmp = tempdir().unwrap();
        assert_eq!(
            detect_with_sysfs_root("/dev/ttyUSB3", tmp.path()),
            Some(KernelDriverClass::UsbSerialBridge)
        );
    }

    #[test]
    fn linux_unrelated_devnode_returns_none() {
        // ttyS0 (real UART, not USB) shouldn't classify as either —
        // the kernel didn't bind it via cdc_acm or usbserial.
        let tmp = tempdir().unwrap();
        assert_eq!(detect_with_sysfs_root("/dev/ttyS0", tmp.path()), None);
    }

    #[test]
    fn linux_classify_driver_unknown_falls_back_to_bridge() {
        // A new bridge driver landing in mainline Linux (e.g.
        // qcserial, mos7720) should classify as a bridge because
        // anything not literally `cdc_acm` is by construction a
        // chip-specific bridge.
        assert_eq!(classify_driver("qcserial"), KernelDriverClass::UsbSerialBridge);
        assert_eq!(classify_driver("pl2303"), KernelDriverClass::UsbSerialBridge);
        assert_eq!(
            classify_driver("totally-not-a-driver"),
            KernelDriverClass::UsbSerialBridge
        );
    }

    #[test]
    fn linux_port_name_stem_strips_dev_prefix() {
        assert_eq!(port_name_stem("/dev/ttyACM0"), Some("ttyACM0"));
        assert_eq!(port_name_stem("ttyACM0"), Some("ttyACM0"));
        assert_eq!(port_name_stem("/some/oddpath/ttyUSB2"), Some("ttyUSB2"));
    }
}
