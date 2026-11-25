//! Present-but-faulty USB device diagnostics for `fbuild port scan`
//! (FastLED/fbuild#1091).
//!
//! Serial enumeration alone cannot explain a board that is plugged in and
//! still missing: an unauthorized, unconfigured, or driverless-CDC node
//! never exposes a tty, so it is absent from the port table and the user is
//! left staring at `no serial ports visible`. Each host reports that
//! differently and both halves already exist in `fbuild-serial`:
//!
//! - Windows: [`fbuild_serial::ports::present_usb_problem_devices`] (PnP
//!   problem codes), wired into `port scan` since FastLED/fbuild#1087.
//! - Linux: [`fbuild_serial::ports::present_usb_problem_devices_linux`]
//!   (sysfs fault signals), delivered by FastLED/fbuild#1207 but — until
//!   this module — never called from any user-facing surface.
//!
//! Both are diagnostics only: an empty list is "the host reported nothing
//! faulty", never "everything is healthy", and neither list is allowed to
//! name a board the host could not actually identify.

use fbuild_serial::ports::UsbProblemDevice;
use fbuild_serial::sysfs_usb::{
    LINUX_PROBLEM_DRIVERLESS_CDC_INTERFACE, LINUX_PROBLEM_UNAUTHORIZED, LINUX_PROBLEM_UNCONFIGURED,
    LinuxUsbProblemDevice,
};

/// Fault devices reported by the host, split by the surface that produced
/// them. Exactly one side is ever non-empty — the other platform's
/// collector returns an empty `Vec` by design.
#[derive(Debug, Default)]
pub(crate) struct UsbProblemSources {
    pub windows: Vec<UsbProblemDevice>,
    pub linux: Vec<LinuxUsbProblemDevice>,
}

/// Ask the host for present-but-faulty USB devices.
///
/// Both calls are cheap, read-only and best-effort; neither can make a scan
/// fail. The Linux call is a straight sysfs read (no subprocess), so the
/// enumeration hot path keeps its latency.
pub(crate) fn collect_usb_problem_sources() -> UsbProblemSources {
    UsbProblemSources {
        windows: fbuild_serial::ports::present_usb_problem_devices(),
        linux: fbuild_serial::ports::present_usb_problem_devices_linux(),
    }
}

/// Render one diagnostic block per host surface that reported faults.
///
/// Windows text stays byte-identical to the pre-#1091 wording so existing
/// logs and expectations keep matching.
pub(crate) fn usb_problem_diagnostics(sources: &UsbProblemSources) -> Vec<String> {
    let mut diagnostics = Vec::new();
    if !sources.windows.is_empty() {
        diagnostics.push(format!(
            "warning: {}",
            format_usb_problem_warning(&sources.windows)
        ));
    }
    if !sources.linux.is_empty() {
        diagnostics.push(format!(
            "warning: {}",
            format_linux_usb_problem_warning(&sources.linux)
        ));
    }
    diagnostics
}

pub(crate) fn format_usb_problem_warning(devices: &[UsbProblemDevice]) -> String {
    use std::fmt::Write as _;
    let mut warning = String::from(
        "Windows reports present USB device(s) with hardware problems; fbuild cannot associate these unidentified nodes with a target board:",
    );
    let mut hub_seen = false;
    for device in devices {
        let location = device
            .location
            .as_deref()
            .map(|value| format!(" at {value}"))
            .unwrap_or_default();
        let topology = match device.behind_external_hub {
            Some(true) => {
                hub_seen = true;
                " behind an external USB hub"
            }
            Some(false) => " on a direct root USB port",
            None => " (USB topology unavailable)",
        };
        let name = device
            .friendly_name
            .as_deref()
            .unwrap_or("Unknown USB device");
        let _ = write!(
            warning,
            "\n  - {name}: Windows problem code {}{location}{topology}",
            device.problem_code
        );
    }
    if hub_seen {
        warning.push_str(
            "\nRecommendation: connect platform USB devices directly to a motherboard USB port; external hubs can introduce power/reset/timing conditions that disrupt USB enumeration.",
        );
    }
    warning
}

/// Render the Linux sysfs fault report.
///
/// Deliberately mirrors the Windows wording where it can: state that the
/// node cannot be tied to a board, name each node with the identifiers the
/// kernel actually exposed, and end with the remedy for the faults seen.
/// A missing product string is reported as an unidentified device — it is
/// never filled in from the VID alone, because a shared VID would then
/// masquerade as a positive identification.
pub(crate) fn format_linux_usb_problem_warning(devices: &[LinuxUsbProblemDevice]) -> String {
    use std::fmt::Write as _;
    let mut warning = String::from(
        "Linux reports present USB device(s) with an observed fault; fbuild cannot associate these nodes with a target board:",
    );
    let mut hub_seen = false;
    let mut remedies: Vec<&'static str> = Vec::new();
    for device in devices {
        let identity = match (device.vendor_id, device.product_id) {
            (Some(vid), Some(pid)) => format!("{vid:04X}:{pid:04X}"),
            _ => "unidentified USB node".to_string(),
        };
        let product = device
            .product
            .as_deref()
            .map(|name| format!(" ({name})"))
            .unwrap_or_default();
        let topology = match device.behind_external_hub {
            Some(true) => {
                hub_seen = true;
                " behind an external USB hub"
            }
            Some(false) => " on a direct root USB port",
            None => " (USB topology unavailable)",
        };
        let _ = write!(
            warning,
            "\n  - {identity}{product} at {}: {} (sysfs code {}){topology}",
            device.sysfs_path, device.reason, device.problem_code
        );
        if let Some(remedy) = remedy_for(device.problem_code) {
            if !remedies.contains(&remedy) {
                remedies.push(remedy);
            }
        }
    }
    if hub_seen {
        warning.push_str(
            "\nRecommendation: connect platform USB devices directly to a motherboard USB port; external hubs can introduce power/reset/timing conditions that disrupt USB enumeration.",
        );
    }
    for remedy in remedies {
        let _ = write!(warning, "\nRecommendation: {remedy}");
    }
    warning
}

/// The one concrete next step for a sysfs fault code.
///
/// Only signals the kernel actually stated get a remedy; an unrecognized
/// code returns `None` rather than guessing at hardware the host did not
/// describe.
fn remedy_for(problem_code: u32) -> Option<&'static str> {
    match problem_code {
        LINUX_PROBLEM_UNAUTHORIZED => Some(
            "authorize the device (`echo 1 | sudo tee /sys/bus/usb/devices/<path>/authorized`, \
             substituting the path named above) or replug it; an unauthorized USB device is \
             never enumerated as a serial port",
        ),
        LINUX_PROBLEM_UNCONFIGURED => Some(
            "replug the board; a USB device with no active configuration never exposes a \
             serial port",
        ),
        LINUX_PROBLEM_DRIVERLESS_CDC_INTERFACE => Some(
            "reload the CDC driver (`sudo modprobe -r cdc_acm && sudo modprobe cdc_acm`) and \
             replug the board; a CDC interface with no bound driver never exposes a serial port",
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linux_device(
        path: &str,
        code: u32,
        reason: &'static str,
        hub: Option<bool>,
    ) -> LinuxUsbProblemDevice {
        LinuxUsbProblemDevice {
            sysfs_path: path.to_string(),
            problem_code: code,
            reason,
            vendor_id: Some(0x303A),
            product_id: Some(0x1001),
            product: Some("USB JTAG/serial debug unit".to_string()),
            behind_external_hub: hub,
            parent_sysfs_path: None,
        }
    }

    fn windows_device(hub: Option<bool>) -> UsbProblemDevice {
        UsbProblemDevice {
            instance_id: r"USB\VID_0000&PID_0002\failure".to_string(),
            problem_code: 43,
            friendly_name: Some(
                "Unknown USB Device (Device Descriptor Request Failed)".to_string(),
            ),
            location: Some("Port_#0001.Hub_#0011".to_string()),
            behind_external_hub: hub,
            parent_instance_id: None,
            device_class: None,
            location_paths: Vec::new(),
        }
    }

    #[test]
    fn linux_warning_names_the_node_the_kernel_reported() {
        let warning = format_linux_usb_problem_warning(&[linux_device(
            "5-2.2.3.3",
            LINUX_PROBLEM_DRIVERLESS_CDC_INTERFACE,
            "CDC interface has no bound driver",
            Some(false),
        )]);
        assert!(warning.contains("Linux reports"), "got: {warning}");
        assert!(warning.contains("303A:1001"), "got: {warning}");
        assert!(
            warning.contains("USB JTAG/serial debug unit"),
            "got: {warning}"
        );
        assert!(warning.contains("at 5-2.2.3.3"), "got: {warning}");
        assert!(
            warning.contains("CDC interface has no bound driver"),
            "got: {warning}"
        );
        assert!(warning.contains("sysfs code 3"), "got: {warning}");
        assert!(warning.contains("cannot associate"), "got: {warning}");
    }

    #[test]
    fn linux_warning_never_invents_a_name_for_an_unidentified_node() {
        let mut device = linux_device(
            "1-1.2",
            LINUX_PROBLEM_UNCONFIGURED,
            "device has no active USB configuration",
            Some(false),
        );
        device.product = None;
        device.vendor_id = None;
        device.product_id = None;
        let warning = format_linux_usb_problem_warning(&[device]);
        assert!(warning.contains("unidentified USB node"), "got: {warning}");
        assert!(
            !warning.contains("0000:0000"),
            "fabricated identity: {warning}"
        );
    }

    #[test]
    fn linux_warning_carries_the_remedy_for_the_observed_code() {
        let unauthorized = format_linux_usb_problem_warning(&[linux_device(
            "1-1",
            LINUX_PROBLEM_UNAUTHORIZED,
            "device is unauthorized",
            Some(false),
        )]);
        assert!(
            unauthorized.contains("authorize the device"),
            "got: {unauthorized}"
        );

        let driverless = format_linux_usb_problem_warning(&[linux_device(
            "1-2",
            LINUX_PROBLEM_DRIVERLESS_CDC_INTERFACE,
            "CDC interface has no bound driver",
            Some(false),
        )]);
        assert!(
            driverless.contains("modprobe -r cdc_acm"),
            "got: {driverless}"
        );
    }

    #[test]
    fn linux_warning_dedupes_a_remedy_shared_by_two_nodes() {
        let warning = format_linux_usb_problem_warning(&[
            linux_device(
                "1-1.1",
                LINUX_PROBLEM_DRIVERLESS_CDC_INTERFACE,
                "CDC interface has no bound driver",
                Some(false),
            ),
            linux_device(
                "1-1.2",
                LINUX_PROBLEM_DRIVERLESS_CDC_INTERFACE,
                "CDC interface has no bound driver",
                Some(false),
            ),
        ]);
        assert_eq!(
            warning.matches("modprobe -r cdc_acm").count(),
            1,
            "remedy repeated: {warning}"
        );
    }

    #[test]
    fn linux_warning_recommends_root_port_only_when_a_hub_is_involved() {
        let behind_hub = format_linux_usb_problem_warning(&[linux_device(
            "1-1.4",
            LINUX_PROBLEM_UNCONFIGURED,
            "device has no active USB configuration",
            Some(true),
        )]);
        assert!(
            behind_hub.contains("behind an external USB hub"),
            "got: {behind_hub}"
        );
        assert!(
            behind_hub.contains("connect platform USB devices directly"),
            "got: {behind_hub}"
        );

        let direct = format_linux_usb_problem_warning(&[linux_device(
            "1-1",
            LINUX_PROBLEM_UNCONFIGURED,
            "device has no active USB configuration",
            Some(false),
        )]);
        assert!(
            direct.contains("on a direct root USB port"),
            "got: {direct}"
        );
        assert!(
            !direct.contains("connect platform USB devices directly"),
            "overclaimed a hub: {direct}"
        );
    }

    #[test]
    fn diagnostics_stay_silent_when_no_host_reports_a_fault() {
        assert!(usb_problem_diagnostics(&UsbProblemSources::default()).is_empty());
    }

    #[test]
    fn diagnostics_include_the_linux_source() {
        let sources = UsbProblemSources {
            windows: Vec::new(),
            linux: vec![linux_device(
                "5-2.2.3.3",
                LINUX_PROBLEM_DRIVERLESS_CDC_INTERFACE,
                "CDC interface has no bound driver",
                Some(false),
            )],
        };
        let diagnostics = usb_problem_diagnostics(&sources);
        assert_eq!(diagnostics.len(), 1, "got: {diagnostics:?}");
        assert!(diagnostics[0].starts_with("warning: Linux reports"));
    }

    #[test]
    fn diagnostics_keep_the_windows_wording_when_windows_reports_the_fault() {
        let sources = UsbProblemSources {
            windows: vec![windows_device(Some(true))],
            linux: Vec::new(),
        };
        let diagnostics = usb_problem_diagnostics(&sources);
        assert_eq!(diagnostics.len(), 1, "got: {diagnostics:?}");
        assert!(diagnostics[0].contains("Windows reports present USB device(s)"));
        assert!(diagnostics[0].contains("Windows problem code 43"));
        assert!(!diagnostics[0].contains("Linux reports"));
    }

    #[test]
    fn diagnostics_report_both_surfaces_without_collapsing_them() {
        let sources = UsbProblemSources {
            windows: vec![windows_device(Some(false))],
            linux: vec![linux_device(
                "1-1",
                LINUX_PROBLEM_UNAUTHORIZED,
                "device is unauthorized",
                Some(false),
            )],
        };
        let diagnostics = usb_problem_diagnostics(&sources);
        assert_eq!(diagnostics.len(), 2, "got: {diagnostics:?}");
        assert!(diagnostics[0].contains("Windows reports"));
        assert!(diagnostics[1].contains("Linux reports"));
    }
}
