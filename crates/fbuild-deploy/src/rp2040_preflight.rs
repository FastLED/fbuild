//! Windows PICOBOOT WinUSB-driver preflight (FastLED/fbuild#1163).
//!
//! Before attempting a picotool-primary RP2040 deploy, classify whether the
//! PICOBOOT composite-interface devnode has a working driver. A devnode
//! stuck at a `CM_PROB_FAILED_INSTALL`-family problem code means picotool
//! cannot possibly open the vendor interface: skip the (10s probe + 60s
//! load) picotool attempt entirely rather than burning that whole timeout
//! budget on a host that can never succeed, and go straight to the
//! mass-storage fallback.

use fbuild_serial::ports::UsbProblemDevice;

use super::picotool::PicotoolTarget;

/// `CM_PROB_FAILED_INSTALL`: Windows Config Manager could not install a
/// driver for the devnode.
const CM_PROB_FAILED_INSTALL: u32 = 28;
/// `CM_PROB_NOT_CONFIGURED`: the devnode has no working driver configured.
/// Treated as part of the FAILED_INSTALL family per the RP2040 driver
/// preflight design (issue #1163).
const CM_PROB_NOT_CONFIGURED: u32 = 1;
/// `CM_PROB_FAILED_ADD`: Windows could not add the device.  Also treated as
/// part of the FAILED_INSTALL family.
const CM_PROB_FAILED_ADD: u32 = 31;

const COMPOSITE_INTERFACE_MARKER: &str = "&MI_";

/// Classification of the PICOBOOT devnode's driver health, computed purely
/// from a snapshot of Windows USB problem devnodes (empty on non-Windows
/// hosts, so this is always `Ready` there).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PicobootPreflight {
    /// No PICOBOOT interface problem devnode was found. This does not prove
    /// the device is present — it may still be mid-enumeration — only that
    /// preflight found no reason to skip the picotool attempt.
    Ready,
    /// The PICOBOOT interface devnode is present but has no working driver
    /// (`CM_PROB_FAILED_INSTALL` family: 28, 1, or 31). picotool cannot open
    /// the vendor interface; skip straight to the mass-storage fallback.
    DriverMissing {
        instance_id: String,
        problem_code: u32,
    },
    /// The PICOBOOT interface devnode reports some other nonzero problem
    /// code. Windows cannot provide a usable vendor interface, so picotool
    /// is skipped in favor of BOOTSEL mass-storage.
    OtherProblem {
        instance_id: String,
        problem_code: u32,
    },
}

/// True for the registry-selected PICOBOOT composite-interface devnode. The
/// `&MI_` marker distinguishes the interface picotool opens from the parent
/// composite device.
fn is_picoboot_interface_devnode(instance_id: &str, target: &PicotoolTarget) -> bool {
    let upper = instance_id.to_ascii_uppercase();
    target.matches_usb_instance(instance_id) && upper.contains(COMPOSITE_INTERFACE_MARKER)
}

fn is_driver_missing_family(problem_code: u32) -> bool {
    matches!(
        problem_code,
        CM_PROB_FAILED_INSTALL | CM_PROB_NOT_CONFIGURED | CM_PROB_FAILED_ADD
    )
}

/// Pure classification (FastLED/fbuild#1163): given a snapshot of present
/// USB problem devnodes, decide whether the PICOBOOT interface has a
/// driver-missing problem that should skip the picotool attempt outright.
pub(super) fn classify_picoboot_preflight(
    devices: &[UsbProblemDevice],
    target: &PicotoolTarget,
) -> PicobootPreflight {
    for device in devices {
        if !is_picoboot_interface_devnode(&device.instance_id, target) {
            continue;
        }
        return if is_driver_missing_family(device.problem_code) {
            PicobootPreflight::DriverMissing {
                instance_id: device.instance_id.clone(),
                problem_code: device.problem_code,
            }
        } else {
            PicobootPreflight::OtherProblem {
                instance_id: device.instance_id.clone(),
                problem_code: device.problem_code,
            }
        };
    }
    PicobootPreflight::Ready
}

/// Diagnostic + actionable guidance for a `DriverMissing` classification,
/// naming the exact devnode and giving the Raspberry Pi-documented WinUSB
/// binding fix. Explicitly states this is not a board fault.
pub(super) fn driver_missing_message(instance_id: &str, problem_code: u32) -> String {
    format!(
        "RP2040 PICOBOOT interface {instance_id} has no working driver (Windows Config Manager problem code {problem_code}); skipping the picotool transport and falling back to the BOOTSEL mass-storage transport. This is not a board fault: bind WinUSB to \"RP2 Boot (Interface 1)\" for this device (e.g. via Zadig or a Windows driver update), then retry to use the picotool transport."
    )
}

pub(super) fn problem_message(preflight: &PicobootPreflight) -> String {
    match preflight {
        PicobootPreflight::Ready => String::new(),
        PicobootPreflight::DriverMissing {
            instance_id,
            problem_code,
        } => driver_missing_message(instance_id, *problem_code),
        PicobootPreflight::OtherProblem {
            instance_id,
            problem_code,
        } => format!(
            "RP-series PICOBOOT interface {instance_id} reports Windows Config Manager problem code {problem_code}; skipping picotool because Windows cannot provide a usable vendor interface. fbuild will use the BOOTSEL mass-storage fallback if it appears."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOOTSEL_INTERFACE: &str = "USB\\VID_2E8A&PID_0003&MI_01\\8&22CF742D&0&0001";
    const RP2350_BOOTSEL_INTERFACE: &str = "USB\\VID_2E8A&PID_000F&MI_01\\8&22CF742D&0&0001";
    const BOOTSEL_COMPOSITE: &str = "USB\\VID_2E8A&PID_0003\\E0C9125B0D9B";
    const UNRELATED: &str = "USB\\VID_25A7&PID_2510\\receiver";

    fn device(instance_id: &str, problem_code: u32) -> UsbProblemDevice {
        UsbProblemDevice {
            instance_id: instance_id.to_string(),
            problem_code,
            friendly_name: Some("RP2 Boot".to_string()),
            location: None,
            behind_external_hub: Some(false),
            parent_instance_id: Some(BOOTSEL_COMPOSITE.to_string()),
            device_class: None,
        }
    }

    fn rp2040_target() -> PicotoolTarget {
        PicotoolTarget::new("test", "2e8a", "0003")
    }

    fn rp2350_target() -> PicotoolTarget {
        PicotoolTarget::new("test", "2e8a", "000f")
    }

    #[test]
    fn empty_snapshot_is_ready() {
        assert_eq!(
            classify_picoboot_preflight(&[], &rp2040_target()),
            PicobootPreflight::Ready
        );
    }

    #[test]
    fn problem_code_28_is_driver_missing() {
        let devices = [device(BOOTSEL_INTERFACE, 28)];
        assert_eq!(
            classify_picoboot_preflight(&devices, &rp2040_target()),
            PicobootPreflight::DriverMissing {
                instance_id: BOOTSEL_INTERFACE.to_string(),
                problem_code: 28,
            }
        );
    }

    #[test]
    fn family_codes_1_and_31_are_driver_missing() {
        for code in [1, 31] {
            let devices = [device(BOOTSEL_INTERFACE, code)];
            assert_eq!(
                classify_picoboot_preflight(&devices, &rp2040_target()),
                PicobootPreflight::DriverMissing {
                    instance_id: BOOTSEL_INTERFACE.to_string(),
                    problem_code: code,
                }
            );
        }
    }

    #[test]
    fn other_problem_code_is_other_problem() {
        let devices = [device(BOOTSEL_INTERFACE, 43)];
        assert_eq!(
            classify_picoboot_preflight(&devices, &rp2040_target()),
            PicobootPreflight::OtherProblem {
                instance_id: BOOTSEL_INTERFACE.to_string(),
                problem_code: 43,
            }
        );
    }

    #[test]
    fn unrelated_devices_are_ignored() {
        let devices = [device(UNRELATED, 28)];
        assert_eq!(
            classify_picoboot_preflight(&devices, &rp2040_target()),
            PicobootPreflight::Ready
        );
    }

    #[test]
    fn composite_parent_without_mi_marker_is_ignored() {
        // The parent composite devnode (no `&MI_`) is not the interface
        // devnode picotool needs a driver on; only the interface node counts.
        let devices = [device(BOOTSEL_COMPOSITE, 28)];
        assert_eq!(
            classify_picoboot_preflight(&devices, &rp2040_target()),
            PicobootPreflight::Ready
        );
    }

    #[test]
    fn rp2350_bootloader_problem_is_not_mistaken_for_rp2040() {
        let devices = [device(RP2350_BOOTSEL_INTERFACE, 43)];
        assert_eq!(
            classify_picoboot_preflight(&devices, &rp2350_target()),
            PicobootPreflight::OtherProblem {
                instance_id: RP2350_BOOTSEL_INTERFACE.to_string(),
                problem_code: 43,
            }
        );
        assert_eq!(
            classify_picoboot_preflight(&devices, &rp2040_target()),
            PicobootPreflight::Ready
        );
    }

    #[test]
    fn driver_missing_message_names_devnode_and_guidance() {
        let message = driver_missing_message(BOOTSEL_INTERFACE, 28);
        assert!(message.contains(BOOTSEL_INTERFACE));
        assert!(message.contains("28"));
        assert!(message.contains("WinUSB"));
        assert!(message.contains("RP2 Boot (Interface 1)"));
        assert!(message.contains("not a board fault"));
    }

    #[test]
    fn other_problem_message_skips_picotool_without_claiming_a_driver_fix() {
        let message = problem_message(&PicobootPreflight::OtherProblem {
            instance_id: RP2350_BOOTSEL_INTERFACE.to_string(),
            problem_code: 43,
        });
        assert!(message.contains("43"));
        assert!(message.contains("skipping picotool"));
        assert!(message.contains("mass-storage fallback"));
    }
}
