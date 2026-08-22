//! Blessed cross-platform serial-port enumeration for fbuild.
//!
//! Enumeration mechanics live behind
//! [`fbuild_core::platform::device::available_serial_ports`] — on Windows that
//! is a SetupAPI fork which (unlike upstream `serialport`) lists serial ports
//! whose PnP devnode reports a **non-OK problem status** (`CM_PROB_*`,
//! phantom, composite `MI_00` interfaces); elsewhere it delegates straight to
//! [`serialport::available_ports`]. This module keeps only the caller-facing
//! policy layer: flattening host observations into [`PortHealth`], and the
//! Linux sysfs health enrichment.
//!
//! ## Why fbuild forks the Windows enumeration
//!
//! `serialport` 4.9's `available_ports()` skips any devnode where
//! `CM_Get_DevNode_Status` reports a problem code other than `0`
//! (`windows/enumerate.rs`: `if port_device.problem() != Some(0) { continue }`).
//! PJRC/Teensy (VID `16C0`) serial functions enumerate on Windows as
//! composite `MI_00` interfaces that commonly report `Status = Unknown`, so
//! upstream drops **every** Teensy COM port — a physically-attached Teensy is
//! invisible to `fbuild port scan` and to the deploy port-discovery snapshot.
//! FastLED/fbuild#962. The fork lives at
//! `crates/fbuild-core/src/platform/windows/device.rs` (MIT/Apache-2.0,
//! upstream serialport) with the single behavioural change of **not filtering
//! on the problem code**, plus population of the composite-interface index
//! (`MI_xx`) so callers can disambiguate a Teensy's Serial vs Serial+MIDI
//! functions.

use fbuild_core::platform::device::{DevNodeObservation, SerialPortFacts, SerialPortTypeFacts};

// The neutral fact records keep their historical caller-facing paths
// (`fbuild_serial::ports::UsbProblemDevice` etc.) — fbuild-deploy and the
// CLI construct them through this module.
pub use fbuild_core::platform::device::is_picotool_reset_compatible_id;
pub use fbuild_core::platform::device::{UsbProblemDevice, UsbResetInterface};

/// Current host health for an enumerated serial endpoint.
///
/// Windows distinguishes a devnode that is present and healthy from a
/// present-but-problematic devnode and a historical phantom record.  Those are
/// facts about the endpoint, not selection policy: callers may keep an
/// unhealthy record for diagnostics while rejecting it for deployment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PortHealth {
    /// The devnode is in the live tree and Config Manager reports no problem.
    HealthyPresent,
    /// The devnode is present but Config Manager returned a non-zero problem.
    PresentProblem {
        problem_code: u32,
        status: Option<u32>,
    },
    /// The devnode is retained by Windows history but is not in the live tree.
    Phantom {
        problem_code: Option<u32>,
        status: Option<u32>,
    },
    /// The host cannot provide equivalent health data (normal off Windows).
    Unknown,
}

impl PortHealth {
    /// True only for states that positively prove the endpoint is unhealthy.
    pub fn is_known_unhealthy(&self) -> bool {
        matches!(self, Self::PresentProblem { .. } | Self::Phantom { .. })
    }

    /// Stable lowercase label for diagnostics and machine-readable callers.
    pub fn label(&self) -> &'static str {
        match self {
            Self::HealthyPresent => "healthy",
            Self::PresentProblem { .. } => "present-problem",
            Self::Phantom { .. } => "phantom",
            Self::Unknown => "unknown",
        }
    }

    /// Whether the endpoint is in the live device tree.
    ///
    /// `Some(false)` is the case worth spelling out: a `Phantom` record is
    /// hardware that is **not attached**, not hardware that is broken. Those
    /// need opposite responses — "plug it in" versus "recover it" — and
    /// conflating them cost a full investigation in FastLED/FastLED#3864.
    /// `None` means the host cannot say (normal off Windows).
    pub fn is_present(&self) -> Option<bool> {
        match self {
            Self::HealthyPresent | Self::PresentProblem { .. } => Some(true),
            Self::Phantom { .. } => Some(false),
            Self::Unknown => None,
        }
    }

    /// Config Manager problem code when the operating system supplied one.
    pub fn problem_code(&self) -> Option<u32> {
        match self {
            Self::PresentProblem { problem_code, .. } => Some(*problem_code),
            Self::Phantom { problem_code, .. } => *problem_code,
            Self::HealthyPresent | Self::Unknown => None,
        }
    }
}

/// A serial endpoint plus the host facts needed to decide whether it is safe
/// to select.  [`Self::info`] retains the upstream `serialport` shape for
/// callers that only need USB identity or a port name.
#[derive(Clone, Debug)]
pub struct DetectedPort {
    pub info: serialport::SerialPortInfo,
    pub health: PortHealth,
    /// Canonical Plug and Play device instance ID when the host exposes one.
    pub instance_id: Option<String>,
    /// Immediate parent device instance ID when the host exposes one.
    pub parent_instance_id: Option<String>,
    /// Full USB ancestor chain, nearest first, when the host exposes one.
    ///
    /// The immediate parent of a composite device is the device itself, not a
    /// hub — anything reasoning about hub-level policy (power management,
    /// topology) needs the whole chain, not just one hop.
    pub ancestor_instance_ids: Vec<String>,
    /// Windows physical USB location paths. Empty when unavailable. These are
    /// identity history only and never make a phantom endpoint selectable.
    pub location_paths: Vec<String>,
}

impl DetectedPort {
    pub fn unknown(info: serialport::SerialPortInfo) -> Self {
        Self {
            info,
            health: PortHealth::Unknown,
            instance_id: None,
            parent_instance_id: None,
            ancestor_instance_ids: Vec::new(),
            location_paths: Vec::new(),
        }
    }
}

fn classify_port_health(observation: DevNodeObservation) -> PortHealth {
    match observation {
        DevNodeObservation::Present {
            status: _,
            problem_code: 0,
        } => PortHealth::HealthyPresent,
        DevNodeObservation::Present {
            status,
            problem_code,
        } => PortHealth::PresentProblem {
            problem_code,
            status: Some(status),
        },
        DevNodeObservation::Phantom => PortHealth::Phantom {
            problem_code: None,
            status: None,
        },
        DevNodeObservation::Unknown => PortHealth::Unknown,
    }
}

fn health_for_endpoint(observation: DevNodeObservation, is_usb: bool) -> PortHealth {
    if is_usb {
        classify_port_health(observation)
    } else {
        // A PnP status for a UART, Bluetooth, or other non-USB endpoint is
        // not equivalent to the USB health contract consumers use for deploy
        // selection. Preserve the cross-platform `Unknown` behavior instead.
        PortHealth::Unknown
    }
}

/// Map neutral host facts back onto the upstream `serialport` shape that
/// [`DetectedPort`] exposes to callers.
fn port_info_from_facts(facts: &SerialPortFacts) -> serialport::SerialPortInfo {
    let port_type = match &facts.port_type {
        SerialPortTypeFacts::Usb(usb) => {
            serialport::SerialPortType::UsbPort(serialport::UsbPortInfo {
                vid: usb.vid,
                pid: usb.pid,
                serial_number: usb.serial_number.clone(),
                manufacturer: usb.manufacturer.clone(),
                product: usb.product.clone(),
                interface: usb.interface,
            })
        }
        // PCI/Bluetooth endpoints carry no facts on any host today.
        SerialPortTypeFacts::Unknown => serialport::SerialPortType::Unknown,
    };
    serialport::SerialPortInfo {
        port_name: facts.port_name.clone(),
        port_type,
    }
}

/// Enumerate every serial port currently visible to the OS.
///
/// Unlike [`serialport::available_ports`], on Windows this includes ports
/// whose devnode status is not "OK" (the Teensy / composite-device case) and
/// preserves that health in the returned record. On Linux, ports backed by a
/// USB tty are additionally enriched with [`PortHealth`] from `sysfs` when
/// `sysfs` gives a concrete healthy/problem signal (see
/// [`crate::sysfs_usb`]); anything ambiguous is left at the default
/// `PortHealth::Unknown`, matching current (pre-#1091) behavior. macOS is
/// unchanged (`Unknown`) — see the module doc comment on `sysfs_usb` for why.
pub fn available_ports() -> serialport::Result<Vec<DetectedPort>> {
    // `serialport::Error: From<std::io::Error>` carries the OS error through.
    let facts = fbuild_core::platform::device::available_serial_ports()?;
    let ports: Vec<DetectedPort> = facts
        .iter()
        .map(|facts| {
            let is_usb = matches!(facts.port_type, SerialPortTypeFacts::Usb(_));
            DetectedPort {
                info: port_info_from_facts(facts),
                health: health_for_endpoint(facts.observation, is_usb),
                instance_id: facts.instance_id.clone(),
                parent_instance_id: facts.parent_instance_id.clone(),
                ancestor_instance_ids: facts.ancestor_instance_ids.clone(),
                location_paths: facts.location_paths.clone(),
            }
        })
        .collect();
    // Only Linux mutates the list (sysfs health enrichment). macOS and other
    // unix targets keep every record at its enumeration-time health. Binding
    // the `mut` inside the cfg keeps non-Linux unix targets from tripping
    // `-D unused-mut`.
    #[cfg(target_os = "linux")]
    let ports = {
        let mut ports = ports;
        enrich_linux_port_health(&mut ports);
        ports
    };
    Ok(ports)
}

/// Overwrite `PortHealth::Unknown` entries with a concrete sysfs-derived
/// signal, for ports whose name is a `/dev/ttyXXX` device. Ports that sysfs
/// has no opinion about (non-USB ttys, ambiguous state) are left untouched.
#[cfg(target_os = "linux")]
fn enrich_linux_port_health(ports: &mut [DetectedPort]) {
    let Some(root) = crate::sysfs_usb::live_root() else {
        return;
    };
    for port in ports.iter_mut() {
        let Some(tty_name) = port.info.port_name.strip_prefix("/dev/") else {
            continue;
        };
        let health = crate::sysfs_usb::health_for_tty_from_root(&root, tty_name);
        if health != PortHealth::Unknown {
            port.health = health;
        }
    }
}

/// Best-effort enumeration of healthy Pico SDK application reset interfaces.
///
/// Windows exposes the function as the standard Raspberry Pi reset-interface
/// compatible ID. Other hosts currently return an empty list; their normal
/// libusb/picotool path remains unchanged.
pub fn present_usb_reset_interfaces() -> Vec<UsbResetInterface> {
    fbuild_core::platform::device::present_usb_reset_interfaces()
}

/// Ask one exact Pico SDK WinUSB reset interface to enter BOOTSEL mode.
///
/// The interface must come from [`present_usb_reset_interfaces`], which binds
/// the live device path to its USB serial and VID/PID before this request is
/// issued. The board may disconnect before Windows reports completion; that
/// is the normal successful shape of the no-data control transfer, so the
/// deployer confirms success by waiting for the target BOOTSEL transport.
pub fn reset_usb_interface_to_bootsel(interface: &UsbResetInterface) -> std::io::Result<()> {
    fbuild_core::platform::device::reset_usb_interface_to_bootsel(interface)
}

/// Best-effort enumeration of present USB devnodes with a non-zero Windows
/// problem code.  This is empty on non-Windows hosts and never makes a port
/// scan fail merely because host diagnostics are unavailable.
///
/// Linux has an equivalent diagnostic (`sysfs`-derived, not Windows PnP
/// problem codes), but the `UsbProblemDevice` shape doesn't fit it honestly
/// (no PnP instance id, no Windows setup class) — see
/// `present_usb_problem_devices_linux` instead (not an intra-doc link: that
/// item is `cfg(target_os = "linux")`, so it does not exist to resolve
/// against when these docs are built on a non-Linux host). macOS has no
/// equivalent implemented yet (IOKit work is out of scope without a macOS
/// host).
pub fn present_usb_problem_devices() -> Vec<UsbProblemDevice> {
    fbuild_core::platform::device::present_usb_problem_devices()
}

/// Linux sibling of [`present_usb_problem_devices`]: `sysfs`-derived USB
/// devices with a concrete, observed fault (unauthorized, unconfigured, or a
/// CDC interface with no bound driver). Diagnostics only — never used to
/// drive `PortHealth` selection directly (that happens per-tty via
/// [`available_ports`]'s Linux enrichment).
#[cfg(target_os = "linux")]
pub fn present_usb_problem_devices_linux() -> Vec<crate::sysfs_usb::LinuxUsbProblemDevice> {
    match crate::sysfs_usb::live_root() {
        Some(root) => crate::sysfs_usb::linux_usb_problem_devices_from_root(&root),
        None => Vec::new(),
    }
}

#[cfg(test)]
mod health_tests {
    use super::*;

    /// A phantom record is hardware that is **not attached**, not hardware
    /// that is faulty. Those need opposite responses, and conflating them
    /// cost a full investigation in FastLED/FastLED#3864.
    #[test]
    fn presence_separates_absent_hardware_from_faulty_hardware() {
        assert_eq!(PortHealth::HealthyPresent.is_present(), Some(true));
        // Present but faulty: still attached — recovery is meaningful here.
        assert_eq!(
            PortHealth::PresentProblem {
                problem_code: 43,
                status: None,
            }
            .is_present(),
            Some(true)
        );
        // Phantom: NOT attached — the remedy is "plug it in", never "recover it".
        assert_eq!(
            PortHealth::Phantom {
                problem_code: None,
                status: None,
            }
            .is_present(),
            Some(false)
        );
        // Off Windows the host cannot say; must not be reported as absent.
        assert_eq!(PortHealth::Unknown.is_present(), None);
    }

    /// `is_known_unhealthy` lumps phantom in with present-problem for deploy
    /// selection, which is correct for *selection* but must not be read as a
    /// presence signal. Pin that the two questions stay distinct.
    #[test]
    fn presence_is_not_the_same_question_as_selectability() {
        let phantom = PortHealth::Phantom {
            problem_code: None,
            status: None,
        };
        let faulty = PortHealth::PresentProblem {
            problem_code: 43,
            status: None,
        };
        assert!(phantom.is_known_unhealthy() && faulty.is_known_unhealthy());
        assert_ne!(phantom.is_present(), faulty.is_present());
    }

    #[test]
    fn classifies_healthy_problem_phantom_and_unknown_endpoints() {
        assert_eq!(
            classify_port_health(DevNodeObservation::Present {
                status: 0,
                problem_code: 0,
            }),
            PortHealth::HealthyPresent
        );
        assert_eq!(
            classify_port_health(DevNodeObservation::Present {
                status: 0x1234,
                problem_code: 31,
            }),
            PortHealth::PresentProblem {
                problem_code: 31,
                status: Some(0x1234),
            }
        );
        assert_eq!(
            classify_port_health(DevNodeObservation::Phantom),
            PortHealth::Phantom {
                problem_code: None,
                status: None,
            }
        );
        assert_eq!(
            classify_port_health(DevNodeObservation::Unknown),
            PortHealth::Unknown
        );
        assert_eq!(
            health_for_endpoint(
                DevNodeObservation::Present {
                    status: 0,
                    problem_code: 31,
                },
                false,
            ),
            PortHealth::Unknown
        );
    }

    #[test]
    fn only_problem_and_phantom_states_are_known_unhealthy() {
        assert!(!PortHealth::HealthyPresent.is_known_unhealthy());
        assert!(!PortHealth::Unknown.is_known_unhealthy());
        assert!(
            PortHealth::PresentProblem {
                problem_code: 43,
                status: Some(0),
            }
            .is_known_unhealthy()
        );
        assert!(
            PortHealth::Phantom {
                problem_code: None,
                status: None,
            }
            .is_known_unhealthy()
        );
    }

    #[test]
    fn recognizes_only_the_pico_sdk_reset_interface_protocol() {
        use fbuild_core::platform::device::is_picotool_reset_compatible_id;
        assert!(is_picotool_reset_compatible_id(
            "usb\\class_FF&subclass_00&prot_01"
        ));
        assert!(!is_picotool_reset_compatible_id(
            "USB\\Class_02&SubClass_02&Prot_01"
        ));
        assert!(!is_picotool_reset_compatible_id(
            "USB\\Class_ff&SubClass_00"
        ));
    }
}
