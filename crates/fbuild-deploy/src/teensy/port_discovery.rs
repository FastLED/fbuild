//! Pre- and post-flash CDC ACM port discovery.
//!
//! Two responsibilities:
//!
//! 1. **Snapshot ports before flash.** Used both as input to the post-flash
//!    "which new port appeared" detector and as the candidate the baud-134
//!    trigger should target when no explicit `--port` was given.
//!
//! 2. **Detect the post-flash CDC ACM port.** Teensy 4 USB-re-enumerates ~1-2 s
//!    after `Booting`; the new device name may differ from the pre-flash one
//!    (`COMyy` → `COMxx` on Windows is the common case). Returning the new
//!    name is what lets the monitor attach to the right device without the
//!    user having to look it up.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use serialport::{SerialPortInfo, SerialPortType};

/// PJRC USB Vendor ID. Stable since 2008; covers every Teensy generation.
pub const PJRC_VID: u16 = 0x16C0;

/// Best-effort enumeration of currently-connected serial ports.
///
/// Errors from the OS are converted to an empty list with a warn-level log —
/// for the snapshot/diff use case, "we couldn't ask the OS" is functionally
/// the same as "no ports", and we don't want a transient enumeration glitch
/// to break the deploy.
///
/// Uses fbuild-serial's blessed enumerator, which (unlike upstream
/// `serialport::available_ports()`) lists Windows ports whose PnP devnode
/// reports a non-OK status — every Teensy composite serial port. Without this
/// the pre/post-flash port diff never sees the Teensy. FastLED/fbuild#962.
pub fn list_ports() -> Vec<SerialPortInfo> {
    match fbuild_serial::ports::available_ports() {
        Ok(ports) => ports,
        Err(e) => {
            tracing::warn!("port enumeration failed: {}", e);
            Vec::new()
        }
    }
}

/// Snapshot the set of port *names* present right now.
///
/// We store only the names (not the full `SerialPortInfo`) because that's all
/// `wait_for_new_cdc_port` needs to compute the diff, and serialising
/// `SerialPortInfo` across thread boundaries can be awkward.
pub fn snapshot_port_names() -> HashSet<String> {
    list_ports().into_iter().map(|p| p.port_name).collect()
}

/// Return true if `port` is reported by `available_ports()` as a PJRC USB
/// device. Used to distinguish "the user gave us a stale CDC name and the
/// device is already HalfKay" from "the user gave us the wrong port entirely".
pub fn is_pjrc_cdc(port: &str) -> bool {
    for info in list_ports() {
        if info.port_name == port {
            if let SerialPortType::UsbPort(usb) = &info.port_type {
                return usb.vid == PJRC_VID;
            }
        }
    }
    false
}

/// Find the first PJRC CDC ACM port currently enumerated, if any.
///
/// Useful when the user did not pass `--port` and we need to pick a target for
/// the baud-134 trigger.
pub fn first_pjrc_cdc_port() -> Option<String> {
    for info in list_ports() {
        if let SerialPortType::UsbPort(usb) = &info.port_type {
            if usb.vid == PJRC_VID {
                return Some(info.port_name);
            }
        }
    }
    None
}

/// Outcome of [`wait_for_new_cdc_port`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NewPortOutcome {
    /// A new port that wasn't in `pre_snapshot` appeared. Carries its name.
    Found(String),
    /// No new port appeared inside `timeout`. The caller should still treat
    /// the deploy as successful (flash completed) but cannot pass a port name
    /// to the monitor.
    TimedOut,
}

/// Poll `available_ports()` until a port that wasn't in `pre_snapshot`
/// appears (preferring PJRC CDC devices), or `timeout` elapses.
///
/// The poll cadence is 100 ms — a deliberate compromise between USB
/// re-enumeration latency on Windows (~1.5 s typical) and CPU spin.
///
/// Preference order for choosing among multiple new ports:
/// 1. New port whose `SerialPortType::UsbPort` matches PJRC_VID.
/// 2. Any new port (lexicographically first).
pub fn wait_for_new_cdc_port(pre_snapshot: &HashSet<String>, timeout: Duration) -> NewPortOutcome {
    let deadline = Instant::now() + timeout;
    let poll = Duration::from_millis(100);

    while Instant::now() < deadline {
        let current = list_ports();
        let mut pjrc_new: Vec<&SerialPortInfo> = Vec::new();
        let mut any_new: Vec<&SerialPortInfo> = Vec::new();
        for info in &current {
            if !pre_snapshot.contains(&info.port_name) {
                any_new.push(info);
                if let SerialPortType::UsbPort(usb) = &info.port_type {
                    if usb.vid == PJRC_VID {
                        pjrc_new.push(info);
                    }
                }
            }
        }
        if let Some(info) = pjrc_new.first() {
            return NewPortOutcome::Found(info.port_name.clone());
        }
        if let Some(info) = any_new.first() {
            return NewPortOutcome::Found(info.port_name.clone());
        }
        std::thread::sleep(poll);
    }
    NewPortOutcome::TimedOut
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_returns_something_or_empty() {
        // CI hosts may have no serial ports at all; just assert the call
        // doesn't panic and returns a (possibly empty) set.
        let _ = snapshot_port_names();
    }

    #[test]
    fn wait_for_new_port_times_out_when_nothing_changes() {
        // With a snapshot that already contains everything we can see, no new
        // port can possibly appear in a 50 ms window.
        let snap = snapshot_port_names();
        let outcome = wait_for_new_cdc_port(&snap, Duration::from_millis(50));
        assert_eq!(outcome, NewPortOutcome::TimedOut);
    }

    #[test]
    fn is_pjrc_cdc_handles_missing_port() {
        // Random port name that does not exist anywhere.
        assert!(!is_pjrc_cdc("/dev/null/not-a-port"));
    }
}
