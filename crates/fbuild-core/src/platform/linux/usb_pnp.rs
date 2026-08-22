//! Selected Linux USB PnP mechanics behind [`crate::platform::device`].
//!
//! USB PnP diagnostics/recovery (problem-device enumeration, the Pico
//! WinUSB BOOTSEL reset surface, CfgMgr32-style inspect/reenumerate/
//! restart) is a Windows-only recovery surface; every entry point here
//! fails closed so callers keep their non-Windows behaviour unchanged.

use std::io;

use crate::platform::device::{UsbPnpDevice, UsbProblemDevice, UsbResetInterface};

pub(crate) fn present_usb_problem_devices() -> Vec<UsbProblemDevice> {
    Vec::new()
}

pub(crate) fn present_usb_reset_interfaces() -> Vec<UsbResetInterface> {
    Vec::new()
}

pub(crate) fn reset_usb_interface_to_bootsel(
    _interface: &UsbResetInterface,
) -> io::Result<()> {
    Err(io::Error::other(
        "Pico WinUSB reset interface is a Windows-only recovery surface",
    ))
}

pub(crate) fn inspect_usb_pnp_device(
    _instance_id: &str,
    _allow_phantom: bool,
) -> Result<UsbPnpDevice, String> {
    Err("USB PnP recovery is a Windows-only surface".to_string())
}

pub(crate) fn reenumerate_usb_parent(_parent_instance_id: &str) -> Result<(), String> {
    Err("USB PnP recovery is a Windows-only surface".to_string())
}

pub(crate) fn restart_usb_device(_instance_id: &str) -> Result<(), String> {
    Err("USB PnP recovery is a Windows-only surface".to_string())
}

pub(crate) fn usb_pnp_post_operation_poll_attempts() -> usize {
    0
}

pub(crate) fn usb_pnp_post_operation_poll_interval() -> std::time::Duration {
    std::time::Duration::from_millis(250)
}
