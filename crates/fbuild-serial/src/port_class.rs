//! Platform-native detection of how a serial port is bound to its OS driver.
//!
//! FastLED/fbuild#895. `serialport-rs`'s enumeration gives us only
//! `(vid, pid, serial_number, manufacturer, product)`. Our
//! [`crate::boards::family_for_vid_pid`] table covers ~7 well-known
//! vendor ranges (Espressif `0x303A:*`, FTDI/CP210x/CH340, Arduino,
//! RP2040, NXP debug probes, Teensy bootloader). Everything outside
//! that table returns `None` and the caller falls back to
//! `(true, true)` — the universal CDC-ACM-bridge-host-ready default.
//!
//! The fallback is unsafe for **any unknown CDC native USB device**.
//! Off-brand ESP32-S3 dev boards (custom OEM VID/PID), uncatalogued
//! native-USB devices, or any future hardware not yet in the VID/PID
//! table all get `(true, true)` — which pulses DTR/RTS in a way that
//! native-USB CDC firmware reads as a reset, clobbering the running
//! sketch on every attach.
//!
//! The host OS already knows whether the port is CDC-class or
//! chip-specific-bridge-class — it picks a different kernel driver
//! based on the USB descriptor's `bInterfaceClass`. This module pulls
//! that signal out per platform and returns a [`PortKernelClass`] that
//! the existing `BoardFamily` fallback chain can consult before
//! defaulting.
//!
//! # Library strategy
//!
//! Researched first:
//!
//! - `serialport-rs` — uses libudev on Linux internally but doesn't
//!   surface class info; would need an upstream patch.
//! - `udev` crate — clean wrapper but requires libudev at runtime.
//!   fbuild deliberately avoids that runtime dep (see Cargo.toml
//!   comment on `usb-ids = "1.2025"` line: "Pure Rust, no libusb /
//!   no udev").
//! - `nusb` / `rusb` — enumerate USB devices but can't link a
//!   `/dev/ttyACM0` path back to its USB device; the linking step is
//!   itself OS-specific.
//! - `usb-enumeration` — returns the same fields as serialport-rs.
//!
//! No library bridges "serial port name → kernel driver class"
//! without the OS-specific linking step. So this module reads the OS
//! authoritative source directly:
//!
//! - **Linux**: pure `std::fs::read_link` on
//!   `/sys/class/tty/<name>/device/driver` — no extra deps.
//! - **macOS**: device-node naming pattern (the IOUSBHostFamily /
//!   vendor-driver naming convention is the canonical signal here).
//!   No `IOKit` query needed for the cases this module needs to
//!   distinguish.
//! - **Windows**: deferred. SetupDi via `windows-sys` is the right
//!   path; documented as a follow-up so this PR stays focused on
//!   Linux/macOS where the gain is concrete. Windows returns `None`
//!   here, which preserves the existing fallback chain — no
//!   regression.
//!
//! # Safety contract
//!
//! Any detection failure for any reason returns `None`. Callers MUST
//! fall through to their existing default behavior on `None` — this
//! module is purely additive. No change to existing call sites'
//! behavior when detection fails (sysfs unmounted, port already
//! disconnected, malformed path, container without /sys, etc.).

/// The kernel's view of which driver class instantiated this port.
///
/// Surfaced from the host OS's authoritative source per platform.
/// Returned only when we can confidently classify; ambiguous cases
/// yield `None` at the top level so callers can keep their existing
/// defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PortKernelClass {
    /// The kernel created this port via its CDC-ACM stack:
    /// - Linux: `cdc_acm.ko` driver (sysfs `/device/driver`
    ///   symlink-target = `cdc_acm`)
    /// - macOS: IOUSBHostFamily's CDC-ACM stack
    ///   (`/dev/cu.usbmodem*` / `/dev/tty.usbmodem*`)
    /// - Windows (deferred): driver service `usbser`
    ///
    /// Typical devices: ESP32-S3 / -C3 native USB, Arduino Leonardo /
    /// Micro / Nano Every / native USB, Teensy 3.x/4.x USB-Serial,
    /// RP2040 stdio_usb, SAMD21/SAMD51 native USB.
    CdcAcm,

    /// The kernel created this port via a chip-specific USB-serial
    /// bridge driver:
    /// - Linux: `usbserial.ko` umbrella (`ftdi_sio`, `cp210x`,
    ///   `ch341`, `pl2303`, etc.) — `/dev/ttyUSB*`
    /// - macOS: vendor driver names — `/dev/cu.usbserial-*`,
    ///   `/dev/cu.SLAB_USBtoUART*`, `/dev/cu.wchusbserial*`,
    ///   `/dev/cu.PL2303-*`
    /// - Windows (deferred): driver services `FTDIBUS`, `silabser`,
    ///   `ch341ser`, etc.
    ///
    /// Typical devices: ESP32-WROOM behind FTDI/CP210x autoreset,
    /// classic Arduino UNO/Mega behind FT232/CH340.
    UsbSerialBridge,
}

/// Detect the port's kernel-side driver class.
///
/// Returns `None` if the port can't be classified (already
/// disconnected, virtual port, container without sysfs/IOReg,
/// unsupported platform). Callers MUST fall through to their existing
/// default on `None` — this function is purely additive.
#[must_use]
pub fn detect_port_kernel_class(port_name: &str) -> Option<PortKernelClass> {
    #[cfg(target_os = "linux")]
    {
        linux::detect(port_name)
    }
    #[cfg(target_os = "macos")]
    {
        macos::detect(port_name)
    }
    #[cfg(target_os = "windows")]
    {
        // Windows path not yet implemented (#895 follow-up: SetupDi
        // via windows-sys to read SPDRP_SERVICE). Returning None here
        // preserves the existing fallback chain so we cannot regress
        // Windows behavior with this PR.
        let _ = port_name;
        None
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = port_name;
        None
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::PortKernelClass;
    use std::path::{Path, PathBuf};

    pub(super) fn detect(port_name: &str) -> Option<PortKernelClass> {
        detect_with_sysfs_root(port_name, Path::new("/sys"))
    }

    /// Linux implementation, factored on `sysfs_root` so unit tests
    /// can point at a temp dir holding a fake sysfs.
    pub(crate) fn detect_with_sysfs_root(
        port_name: &str,
        sysfs_root: &Path,
    ) -> Option<PortKernelClass> {
        let bare = port_name_stem(port_name)?;

        // 1. Authoritative path: read the driver symlink.
        //    /sys/class/tty/<name>/device/driver  -> a driver dir
        //    e.g. -> .../bus/usb-serial/drivers/cdc_acm
        //          -> .../bus/usb-serial/drivers/ftdi_sio
        //          -> .../bus/usb-serial/drivers/cp210x
        if let Some(driver_name) =
            read_driver_symlink_name(sysfs_root, bare)
        {
            return Some(classify_driver(&driver_name));
        }

        // 2. Fallback: the kernel's device-node naming convention.
        //    `ttyACM*` is created by `cdc_acm.ko`; `ttyUSB*` by
        //    `usbserial.ko`. If sysfs isn't readable for some reason
        //    (container, permissions), the name is still a strong
        //    signal because the kernel picks the prefix based on
        //    which driver claimed the device.
        classify_by_devnode_name(bare)
    }

    fn read_driver_symlink_name(sysfs_root: &Path, port_stem: &str) -> Option<String> {
        let driver_link: PathBuf = sysfs_root
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

    /// Strip `/dev/` (or `/devices/` in some odd configurations) and
    /// return the bare port name, e.g. `ttyACM0`.
    pub(super) fn port_name_stem(port_name: &str) -> Option<&str> {
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
    /// `cdc_acm` is the unambiguous CDC-ACM driver. The rest of the
    /// usb-serial bridge drivers are bridge chips; the canonical set
    /// is what's enabled in mainline Linux's
    /// `drivers/usb/serial/*.c`. Anything unknown defaults to bridge
    /// because the kernel only invokes `cdc_acm` for actual CDC class
    /// — every other driver is a chip-specific bridge by construction.
    pub(crate) fn classify_driver(driver_name: &str) -> PortKernelClass {
        match driver_name {
            "cdc_acm" => PortKernelClass::CdcAcm,
            _ => PortKernelClass::UsbSerialBridge,
        }
    }

    /// Fall back to device-node naming when sysfs isn't readable.
    pub(crate) fn classify_by_devnode_name(port_stem: &str) -> Option<PortKernelClass> {
        if port_stem.starts_with("ttyACM") {
            return Some(PortKernelClass::CdcAcm);
        }
        if port_stem.starts_with("ttyUSB") {
            return Some(PortKernelClass::UsbSerialBridge);
        }
        None
    }

    #[cfg(test)]
    pub(super) use port_name_stem as port_name_stem_for_tests;
}

#[cfg(target_os = "macos")]
mod macos {
    use super::PortKernelClass;

    pub(super) fn detect(port_name: &str) -> Option<PortKernelClass> {
        classify_macos_devnode(port_name)
    }

    /// macOS device-node naming is set per-driver and is the canonical
    /// signal:
    ///
    /// - `IOUSBHostFamily`'s CDC-ACM stack publishes `/dev/cu.usbmodem*`
    ///   and `/dev/tty.usbmodem*`. The kernel picks this when the
    ///   device exposes the CDC class.
    /// - Vendor drivers publish their own prefixes:
    ///   - `FTDIUSBSerialDriver` -> `cu.usbserial-*`
    ///   - `SiLabsUSBDriver` (CP210x) -> `cu.SLAB_USBtoUART*` (and on
    ///     newer macOS, `cu.usbserial-*` via the Apple-shipped
    ///     `AppleUSBCHCOM` driver)
    ///   - WCH (CH340/CH341) -> `cu.wchusbserial*`
    ///   - Prolific (PL2303) -> `cu.PL2303-*` or `cu.usbserial-*`
    ///
    /// Any name we don't recognize returns `None` so the caller falls
    /// back to its existing default — same safety contract as the
    /// rest of this module.
    pub(crate) fn classify_macos_devnode(port_name: &str) -> Option<PortKernelClass> {
        // Strip `/dev/` prefix if present, then strip the cu./tty.
        // disambiguation prefix.
        let bare = port_name.strip_prefix("/dev/").unwrap_or(port_name);
        let suffix = bare
            .strip_prefix("cu.")
            .or_else(|| bare.strip_prefix("tty."))
            .unwrap_or(bare);

        if suffix.starts_with("usbmodem") {
            return Some(PortKernelClass::CdcAcm);
        }
        if suffix.starts_with("usbserial-")
            || suffix.starts_with("usbserial.")
            || suffix.starts_with("SLAB_USBtoUART")
            || suffix.starts_with("wchusbserial")
            || suffix.starts_with("PL2303")
        {
            return Some(PortKernelClass::UsbSerialBridge);
        }
        None
    }
}

// ---------- Cross-platform unit tests for pure-function logic ----------

#[cfg(test)]
mod tests {
    use super::*;

    // --- Linux ---

    #[cfg(target_os = "linux")]
    mod linux_tests {
        use super::*;
        use std::fs;
        use std::os::unix::fs::symlink;
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
            fs::create_dir_all(&device_dir).unwrap();
            // The driver target dir must exist (the actual sysfs has
            // it; read_link only follows the symlink, but having a
            // real target dir matches the production shape).
            let driver_dir = sysfs_root
                .join("bus")
                .join("usb-serial")
                .join("drivers")
                .join(driver_name);
            fs::create_dir_all(&driver_dir).unwrap();
            symlink(&driver_dir, device_dir.join("driver")).unwrap();
        }

        #[test]
        fn linux_sysfs_cdc_acm_driver_is_cdc() {
            let tmp = tempdir().unwrap();
            build_fake_sysfs_tree(tmp.path(), "ttyACM0", "cdc_acm");
            assert_eq!(
                linux::detect_with_sysfs_root("/dev/ttyACM0", tmp.path()),
                Some(PortKernelClass::CdcAcm)
            );
        }

        #[test]
        fn linux_sysfs_ftdi_driver_is_bridge() {
            let tmp = tempdir().unwrap();
            build_fake_sysfs_tree(tmp.path(), "ttyUSB0", "ftdi_sio");
            assert_eq!(
                linux::detect_with_sysfs_root("/dev/ttyUSB0", tmp.path()),
                Some(PortKernelClass::UsbSerialBridge)
            );
        }

        #[test]
        fn linux_sysfs_cp210x_driver_is_bridge() {
            let tmp = tempdir().unwrap();
            build_fake_sysfs_tree(tmp.path(), "ttyUSB1", "cp210x");
            assert_eq!(
                linux::detect_with_sysfs_root("/dev/ttyUSB1", tmp.path()),
                Some(PortKernelClass::UsbSerialBridge)
            );
        }

        #[test]
        fn linux_sysfs_ch341_driver_is_bridge() {
            let tmp = tempdir().unwrap();
            build_fake_sysfs_tree(tmp.path(), "ttyUSB2", "ch341");
            assert_eq!(
                linux::detect_with_sysfs_root("/dev/ttyUSB2", tmp.path()),
                Some(PortKernelClass::UsbSerialBridge)
            );
        }

        #[test]
        fn linux_devnode_name_acm_is_cdc() {
            // No sysfs entry exists at all → fall back to devnode
            // name. ttyACM* is created only by cdc_acm so this is
            // reliable.
            let tmp = tempdir().unwrap();
            assert_eq!(
                linux::detect_with_sysfs_root("/dev/ttyACM7", tmp.path()),
                Some(PortKernelClass::CdcAcm)
            );
        }

        #[test]
        fn linux_devnode_name_usb_is_bridge() {
            let tmp = tempdir().unwrap();
            assert_eq!(
                linux::detect_with_sysfs_root("/dev/ttyUSB3", tmp.path()),
                Some(PortKernelClass::UsbSerialBridge)
            );
        }

        #[test]
        fn linux_unrelated_devnode_returns_none() {
            // ttyS0 (real UART, not USB) shouldn't classify as
            // either — the kernel didn't bind it via cdc_acm or
            // usbserial.
            let tmp = tempdir().unwrap();
            assert_eq!(linux::detect_with_sysfs_root("/dev/ttyS0", tmp.path()), None);
        }

        #[test]
        fn linux_classify_driver_unknown_falls_back_to_bridge() {
            // A new bridge driver landing in mainline Linux (e.g.
            // qcserial, mos7720) should classify as a bridge because
            // anything not literally `cdc_acm` is by construction a
            // chip-specific bridge.
            assert_eq!(
                linux::classify_driver("qcserial"),
                PortKernelClass::UsbSerialBridge
            );
            assert_eq!(
                linux::classify_driver("pl2303"),
                PortKernelClass::UsbSerialBridge
            );
            assert_eq!(
                linux::classify_driver("totally-not-a-driver"),
                PortKernelClass::UsbSerialBridge
            );
        }

        #[test]
        fn linux_port_name_stem_strips_dev_prefix() {
            assert_eq!(
                linux::port_name_stem_for_tests("/dev/ttyACM0"),
                Some("ttyACM0")
            );
            assert_eq!(linux::port_name_stem_for_tests("ttyACM0"), Some("ttyACM0"));
            assert_eq!(
                linux::port_name_stem_for_tests("/some/oddpath/ttyUSB2"),
                Some("ttyUSB2")
            );
        }
    }

    // --- macOS ---

    #[cfg(target_os = "macos")]
    mod macos_tests {
        use super::*;

        #[test]
        fn macos_usbmodem_is_cdc() {
            assert_eq!(
                macos::classify_macos_devnode("/dev/cu.usbmodem14101"),
                Some(PortKernelClass::CdcAcm)
            );
            assert_eq!(
                macos::classify_macos_devnode("/dev/tty.usbmodem14101"),
                Some(PortKernelClass::CdcAcm)
            );
        }

        #[test]
        fn macos_ftdi_usbserial_is_bridge() {
            assert_eq!(
                macos::classify_macos_devnode("/dev/cu.usbserial-A1234567"),
                Some(PortKernelClass::UsbSerialBridge)
            );
            assert_eq!(
                macos::classify_macos_devnode("/dev/tty.usbserial-FTDI"),
                Some(PortKernelClass::UsbSerialBridge)
            );
        }

        #[test]
        fn macos_slab_cp210x_is_bridge() {
            assert_eq!(
                macos::classify_macos_devnode("/dev/cu.SLAB_USBtoUART"),
                Some(PortKernelClass::UsbSerialBridge)
            );
        }

        #[test]
        fn macos_wch_ch340_is_bridge() {
            assert_eq!(
                macos::classify_macos_devnode("/dev/cu.wchusbserial1410"),
                Some(PortKernelClass::UsbSerialBridge)
            );
        }

        #[test]
        fn macos_pl2303_is_bridge() {
            assert_eq!(
                macos::classify_macos_devnode("/dev/cu.PL2303-XYZ"),
                Some(PortKernelClass::UsbSerialBridge)
            );
        }

        #[test]
        fn macos_bare_name_without_dev_prefix() {
            // Caller passed in a bare name — should still work.
            assert_eq!(
                macos::classify_macos_devnode("cu.usbmodem1101"),
                Some(PortKernelClass::CdcAcm)
            );
        }

        #[test]
        fn macos_unrelated_returns_none() {
            // /dev/cu.Bluetooth-Incoming-Port shouldn't be classified
            // as either CDC or bridge — it's not USB.
            assert_eq!(
                macos::classify_macos_devnode("/dev/cu.Bluetooth-Incoming-Port"),
                None
            );
            // Stray random name returns None too.
            assert_eq!(
                macos::classify_macos_devnode("/dev/cu.random-thing"),
                None
            );
        }
    }

    // --- Cross-platform: Windows path is a no-op for now ---

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_returns_none_for_now() {
        // Documented: Windows SetupDi detection is a follow-up. Until
        // it lands, the path returns None so the existing fallback
        // chain stays in charge — no regression risk.
        assert_eq!(detect_port_kernel_class("COM3"), None);
        assert_eq!(detect_port_kernel_class("COM42"), None);
    }
}
