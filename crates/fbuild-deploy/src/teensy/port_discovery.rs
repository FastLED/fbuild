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

use fbuild_serial::ports::DetectedPort;
use serialport::SerialPortType;

/// Test-only identity fixture. Production Teensy classification is supplied by
/// the verified FastLED/boards USB transport profiles.
#[cfg(test)]
pub const PJRC_VID: u16 = 0x16C0;

fn profile_is_teensy_runtime(profile: &fbuild_core::usb::profiles::UsbTransportProfile) -> bool {
    use fbuild_core::usb::profiles::{UsbDeviceRole, UsbPurpose};

    profile.purpose == UsbPurpose::Runtime
        && profile.role == UsbDeviceRole::RuntimeCdc
        && (profile.platform.as_deref() == Some("teensy")
            || profile.family.as_deref() == Some("teensy"))
}

fn is_teensy_runtime_identity(vid: u16, pid: u16) -> bool {
    #[cfg(test)]
    {
        vid == PJRC_VID && pid != 0x0478
    }
    #[cfg(not(test))]
    {
        fbuild_core::usb::profiles::profiles_for(vid, pid)
            .iter()
            .any(profile_is_teensy_runtime)
    }
}

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
pub fn list_ports() -> Vec<DetectedPort> {
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
    list_ports().into_iter().map(|p| p.info.port_name).collect()
}

/// Return true if `port` is reported by `available_ports()` as a PJRC USB
/// device. Used to distinguish "the user gave us a stale CDC name and the
/// device is already HalfKay" from "the user gave us the wrong port entirely".
pub fn is_pjrc_cdc(port: &str) -> bool {
    for info in list_ports() {
        if info.info.port_name == port {
            if let SerialPortType::UsbPort(usb) = &info.info.port_type {
                return is_teensy_runtime_identity(usb.vid, usb.pid);
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
        if info.health.is_known_unhealthy() {
            continue;
        }
        if let SerialPortType::UsbPort(usb) = &info.info.port_type {
            if is_teensy_runtime_identity(usb.vid, usb.pid) {
                return Some(info.info.port_name);
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
/// 1. New port whose USB identity has a Teensy runtime profile.
/// 2. Any new port (lexicographically first).
pub fn wait_for_new_cdc_port(pre_snapshot: &HashSet<String>, timeout: Duration) -> NewPortOutcome {
    let deadline = Instant::now() + timeout;
    let poll = Duration::from_millis(100);

    while Instant::now() < deadline {
        let current = list_ports();
        let mut pjrc_new: Vec<&DetectedPort> = Vec::new();
        let mut any_new: Vec<&DetectedPort> = Vec::new();
        for info in &current {
            if !pre_snapshot.contains(&info.info.port_name) && !info.health.is_known_unhealthy() {
                any_new.push(info);
                if let SerialPortType::UsbPort(usb) = &info.info.port_type {
                    if is_teensy_runtime_identity(usb.vid, usb.pid) {
                        pjrc_new.push(info);
                    }
                }
            }
        }
        if let Some(info) = pjrc_new.first() {
            return NewPortOutcome::Found(info.info.port_name.clone());
        }
        if let Some(info) = any_new.first() {
            return NewPortOutcome::Found(info.info.port_name.clone());
        }
        std::thread::sleep(poll);
    }
    NewPortOutcome::TimedOut
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn teensy_runtime_classification_uses_profile_semantics() {
        use fbuild_core::usb::profiles::{
            UsbDeviceRole, UsbIdentityMatch, UsbProfileProvenance, UsbPurpose, UsbTransportProfile,
        };
        let profile = UsbTransportProfile {
            identity_match: UsbIdentityMatch {
                vid: "feed".to_string(),
                pid: Some("c0de".to_string()),
                pid_mask: None,
            },
            purpose: UsbPurpose::Runtime,
            role: UsbDeviceRole::RuntimeCdc,
            transport: "usb".to_string(),
            reset: "touch-1200".to_string(),
            handoff: "bootloader".to_string(),
            platform: Some("teensy".to_string()),
            family: Some("teensy".to_string()),
            generation: None,
            interface: Some("cdc".to_string()),
            provenance: UsbProfileProvenance {
                source_url: "test://fixture".to_string(),
                source_revision: "a".repeat(40),
                source_class: "test".to_string(),
            },
            priority: 100,
            allow_ambiguous: false,
        };
        assert!(profile_is_teensy_runtime(&profile));
    }

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
