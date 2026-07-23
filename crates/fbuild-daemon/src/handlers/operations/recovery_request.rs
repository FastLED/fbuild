//! Compose a typed exact-device USB recovery request (FastLED/fbuild#1152).
//!
//! The daemon never elevates and never performs PnP writes. When an RP2040
//! deploy either failed outright or flashed without recovering a runtime CDC
//! endpoint, it composes a [`UsbRecoveryRequest`] from **fresh** scan facts
//! and returns it inside the deploy response. The normal CLI applies the
//! `--admin`/`--no-admin` policy and, at most once, launches the #1148
//! one-shot elevated helper, which re-proves the identity live before the
//! single allowlisted PnP operation.

use crate::device_manager::DeviceState;
use fbuild_core::usb::{UNCLASSED_DEVICE_CLASS, UsbRecoveryRequest};
use fbuild_serial::ports::UsbProblemDevice;

/// Pick the exact-device recovery target from fresh scan facts.
///
/// Preference order:
/// 1. A present BOOTSEL problem **interface** devnode (`...&MI_xx\...`) of a
///    matching UF2-bootloader identity — it owns the transport the deploy
///    needs next (synthetic volume / PICOBOOT), and its verified parent
///    composite is the restart target that can actually remount the volume.
/// 2. The known-unhealthy (phantom / present-problem) runtime CDC record of
///    a matching runtime identity.
///
/// Records without a canonical instance ID or a Config-Manager-proved parent
/// are skipped: the helper would fail closed on them anyway, and a typed
/// request must never be composed from guesses.
pub(super) fn compose_rp2040_recovery_request(
    devices: &[DeviceState],
    problem_devices: &[UsbProblemDevice],
    operation_id: &str,
    flash_completed: bool,
    bootloader_match: impl Fn(u16, u16) -> bool,
    runtime_match: impl Fn(u16, u16) -> bool,
) -> Option<UsbRecoveryRequest> {
    for device in problem_devices {
        let Some((vid, pid)) = parse_usb_vid_pid(&device.instance_id) else {
            continue;
        };
        if !bootloader_match(vid, pid) || !is_composite_interface(&device.instance_id) {
            continue;
        }
        let Some(parent) = device.parent_instance_id.clone() else {
            continue;
        };
        return Some(UsbRecoveryRequest {
            operation_id: operation_id.to_string(),
            instance_id: device.instance_id.clone(),
            expected_class: device
                .device_class
                .clone()
                .unwrap_or_else(|| UNCLASSED_DEVICE_CLASS.to_string()),
            expected_serial: serial_from_matching_parent(&device.instance_id, &parent),
            parent_instance_id: Some(parent),
            expected_vid: vid,
            expected_pid: pid,
            problem_code: Some(device.problem_code),
            flash_completed,
        });
    }
    for device in devices {
        if !device.port_health.is_known_unhealthy() {
            continue;
        }
        let (Some(vid), Some(pid)) = (device.vid, device.pid) else {
            continue;
        };
        if !runtime_match(vid, pid) {
            continue;
        }
        let Some(instance_id) = device.instance_id.clone() else {
            continue;
        };
        let Some(parent) = device.parent_instance_id.clone() else {
            continue;
        };
        return Some(UsbRecoveryRequest {
            operation_id: operation_id.to_string(),
            instance_id,
            // Runtime CDC devnodes are `usbser`-class serial ports; the
            // helper revalidates this against the live (or phantom) record.
            expected_class: "Ports".to_string(),
            parent_instance_id: Some(parent),
            expected_vid: vid,
            expected_pid: pid,
            expected_serial: device.serial_number.clone(),
            problem_code: device.port_health.problem_code(),
            flash_completed,
        });
    }
    None
}

fn is_composite_interface(instance_id: &str) -> bool {
    instance_id.to_ascii_uppercase().contains("&MI_")
}

/// Parse `USB\VID_xxxx&PID_xxxx...` identity from a PnP instance ID.
fn parse_usb_vid_pid(instance_id: &str) -> Option<(u16, u16)> {
    let upper = instance_id.to_ascii_uppercase();
    let rest = upper.strip_prefix("USB\\")?;
    let vid_start = rest.find("VID_")? + 4;
    let pid_start = rest.find("PID_")? + 4;
    let vid = u16::from_str_radix(rest.get(vid_start..vid_start + 4)?, 16).ok()?;
    let pid = u16::from_str_radix(rest.get(pid_start..pid_start + 4)?, 16).ok()?;
    Some((vid, pid))
}

/// The composite parent's third instance segment is the device serial, but
/// only when the parent shares the child's VID/PID (interface devnodes get
/// synthetic instance suffixes; the serial lives on the parent).
fn serial_from_matching_parent(child_instance: &str, parent_instance: &str) -> Option<String> {
    let child = parse_usb_vid_pid(child_instance)?;
    let parent = parse_usb_vid_pid(parent_instance)?;
    if child != parent {
        return None;
    }
    parent_instance
        .split('\\')
        .nth(2)
        .filter(|segment| !segment.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fbuild_serial::ports::PortHealth;
    use std::collections::HashMap;

    const BOOTSEL_INTERFACE: &str = "USB\\VID_2E8A&PID_0003&MI_01\\8&22CF742D&0&0001";
    const BOOTSEL_COMPOSITE: &str = "USB\\VID_2E8A&PID_0003\\E0C9125B0D9B";
    const PHANTOM_CDC: &str = "USB\\VID_2E8A&PID_000A\\5303284720C4641C";

    fn phantom_cdc_device() -> DeviceState {
        DeviceState {
            device_id: "2e8a:000a".to_string(),
            port: "COM12".to_string(),
            description: "USB Serial Device".to_string(),
            vid: Some(0x2E8A),
            pid: Some(0x000A),
            vendor_name: None,
            product_name: None,
            is_cdc: Some(true),
            serial_number: Some("5303284720C4641C".to_string()),
            port_health: PortHealth::Phantom {
                problem_code: Some(45),
                status: None,
            },
            instance_id: Some(PHANTOM_CDC.to_string()),
            parent_instance_id: Some("USB\\ROOT_HUB30\\5&23f8e3f5&0&0".to_string()),
            previous_port: None,
            exclusive_lease: None,
            monitor_leases: HashMap::new(),
            last_seen_at: 0.0,
            is_connected: true,
            trusted_firmware: None,
            last_disconnect_at: None,
        }
    }

    fn bootsel_problem_interface() -> UsbProblemDevice {
        UsbProblemDevice {
            instance_id: BOOTSEL_INTERFACE.to_string(),
            problem_code: 28,
            friendly_name: Some("RP2 Boot".to_string()),
            location: None,
            behind_external_hub: Some(false),
            parent_instance_id: Some(BOOTSEL_COMPOSITE.to_string()),
            device_class: None,
        }
    }

    #[test]
    fn bootsel_problem_interface_is_preferred_over_phantom_cdc() {
        let request = compose_rp2040_recovery_request(
            &[phantom_cdc_device()],
            &[bootsel_problem_interface()],
            "deploy-1",
            false,
            |_, _| true,
            |_, _| true,
        )
        .expect("a typed request must be composed");
        assert_eq!(request.instance_id, BOOTSEL_INTERFACE);
        assert_eq!(
            request.parent_instance_id.as_deref(),
            Some(BOOTSEL_COMPOSITE)
        );
        assert_eq!(request.expected_class, UNCLASSED_DEVICE_CLASS);
        assert_eq!(request.expected_serial.as_deref(), Some("E0C9125B0D9B"));
        assert_eq!(
            (request.expected_vid, request.expected_pid),
            (0x2E8A, 0x0003)
        );
        assert_eq!(request.problem_code, Some(28));
        assert!(!request.flash_completed);
        assert!(request.has_canonical_identity());
    }

    #[test]
    fn phantom_cdc_yields_a_typed_exact_device_request() {
        let request = compose_rp2040_recovery_request(
            &[phantom_cdc_device()],
            &[],
            "deploy-2",
            true,
            |_, _| true,
            |_, _| true,
        )
        .expect("the phantom runtime CDC must compose a request");
        assert_eq!(request.instance_id, PHANTOM_CDC);
        assert_eq!(request.expected_class, "Ports");
        assert_eq!(request.expected_serial.as_deref(), Some("5303284720C4641C"));
        assert_eq!(
            (request.expected_vid, request.expected_pid),
            (0x2E8A, 0x000A)
        );
        assert_eq!(request.problem_code, Some(45));
        assert!(request.flash_completed);
        assert!(request.has_canonical_identity());
    }

    #[test]
    fn healthy_devices_and_unrelated_problems_compose_nothing() {
        let mut healthy = phantom_cdc_device();
        healthy.port_health = PortHealth::HealthyPresent;
        let mut unrelated = bootsel_problem_interface();
        unrelated.instance_id = "USB\\VID_25A7&PID_2510\\receiver".to_string();
        assert_eq!(
            compose_rp2040_recovery_request(
                &[healthy],
                &[unrelated],
                "deploy-3",
                false,
                |vid, _| vid == 0x2E8A,
                |vid, _| vid == 0x2E8A,
            ),
            None
        );
    }

    #[test]
    fn records_without_canonical_identity_or_parent_are_skipped() {
        let mut no_instance = phantom_cdc_device();
        no_instance.instance_id = None;
        let mut no_parent = phantom_cdc_device();
        no_parent.parent_instance_id = None;
        let mut interface_without_parent = bootsel_problem_interface();
        interface_without_parent.parent_instance_id = None;
        assert_eq!(
            compose_rp2040_recovery_request(
                &[no_instance, no_parent],
                &[interface_without_parent],
                "deploy-4",
                false,
                |_, _| true,
                |_, _| true,
            ),
            None
        );
    }

    #[test]
    fn parent_serial_requires_matching_vid_pid() {
        assert_eq!(
            serial_from_matching_parent(BOOTSEL_INTERFACE, BOOTSEL_COMPOSITE).as_deref(),
            Some("E0C9125B0D9B")
        );
        assert_eq!(
            serial_from_matching_parent(BOOTSEL_INTERFACE, "USB\\ROOT_HUB30\\5&23f8e3f5&0&0"),
            None
        );
    }
}
