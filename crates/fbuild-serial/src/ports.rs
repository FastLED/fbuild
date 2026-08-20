//! Blessed cross-platform serial-port enumeration for fbuild.
//!
//! On non-Windows platforms this delegates straight to
//! [`serialport::available_ports`]. On Windows it replaces the upstream
//! enumeration so that serial ports whose PnP devnode reports a **non-OK
//! problem status** (`CM_PROB_*`, phantom, composite `MI_00` interfaces)
//! are still listed.
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
//! FastLED/fbuild#962.
//!
//! This module is a fork of serialport's `windows/enumerate.rs` (MIT/Apache-2.0)
//! with the single behavioural change of **not filtering on the problem code**,
//! plus population of the composite-interface index (`MI_xx`) so callers can
//! disambiguate a Teensy's Serial vs Serial+MIDI functions.

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

// The classification pipeline below is fed by the Windows PnP enumeration in
// `imp` and exercised cross-platform by `health_tests`; on non-Windows,
// non-test builds it has no production caller, which `-D warnings` would
// otherwise turn into a hard error.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PnpObservation {
    Present { status: u32, problem_code: u32 },
    Phantom,
    Unknown,
}

#[cfg_attr(not(windows), allow(dead_code))]
fn classify_port_health(observation: PnpObservation) -> PortHealth {
    match observation {
        PnpObservation::Present {
            status: _,
            problem_code: 0,
        } => PortHealth::HealthyPresent,
        PnpObservation::Present {
            status,
            problem_code,
        } => PortHealth::PresentProblem {
            problem_code,
            status: Some(status),
        },
        PnpObservation::Phantom => PortHealth::Phantom {
            problem_code: None,
            status: None,
        },
        PnpObservation::Unknown => PortHealth::Unknown,
    }
}

#[cfg_attr(not(windows), allow(dead_code))]
fn health_for_endpoint(observation: PnpObservation, is_usb: bool) -> PortHealth {
    if is_usb {
        classify_port_health(observation)
    } else {
        // A PnP status for a UART, Bluetooth, or other non-USB endpoint is
        // not equivalent to the USB health contract consumers use for deploy
        // selection. Preserve the cross-platform `Unknown` behavior instead.
        PortHealth::Unknown
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
    #[cfg(windows)]
    {
        imp::available_ports()
    }
    #[cfg(not(windows))]
    {
        let ports: Vec<DetectedPort> = serialport::available_ports()?
            .into_iter()
            .map(DetectedPort::unknown)
            .collect();
        // Only Linux mutates the list (sysfs health enrichment). Binding the
        // `mut` inside the cfg keeps non-Linux unix targets — macOS, the BSDs —
        // from tripping `-D unused-mut`.
        #[cfg(target_os = "linux")]
        let ports = {
            let mut ports = ports;
            enrich_linux_port_health(&mut ports);
            ports
        };
        Ok(ports)
    }
}

/// Overwrite `PortHealth::Unknown` entries with a concrete sysfs-derived
/// signal, for ports whose name is a `/dev/ttyXXX` device. Ports that sysfs
/// has no opinion about (non-USB ttys, ambiguous state) are left untouched.
#[cfg(target_os = "linux")]
fn enrich_linux_port_health(ports: &mut [DetectedPort]) {
    let root = crate::sysfs_usb::live_root();
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

/// A USB device that Windows has instantiated but could not start normally.
///
/// These nodes may not have a usable VID/PID or serial number (for example,
/// Windows reports a descriptor failure as `VID_0000&PID_0002`).  The result
/// is deliberately diagnostic only: callers must not treat one of these
/// nodes as a particular target board.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UsbProblemDevice {
    pub instance_id: String,
    pub problem_code: u32,
    pub friendly_name: Option<String>,
    pub location: Option<String>,
    /// `Some(true)` means a USB device ancestor exists before the root hub;
    /// `Some(false)` means the node reaches a root hub directly; `None` means
    /// the host could not provide enough ancestry to classify it.
    pub behind_external_hub: Option<bool>,
    /// Immediate parent instance ID, when Config Manager can prove one.
    /// Needed to compose an exact-device `UsbRecoveryRequest` for a problem
    /// interface devnode (FastLED/fbuild#1152).
    pub parent_instance_id: Option<String>,
    /// Windows device class (e.g. `Ports`, `USB`); `None` for driverless
    /// devnodes that never got a class assigned.
    pub device_class: Option<String>,
    /// Windows physical USB location paths for exact device-local
    /// correlation. Human-readable `location` is not stable enough for this.
    pub location_paths: Vec<String>,
}

/// A healthy, present Pico SDK application-mode USB reset interface.
///
/// Arduino-Pico exposes this WinUSB function when `ENABLE_PICOTOOL_USB` is
/// enabled. It remains independently addressable when the sibling CDC
/// interface is missing or unusable, which lets the RP deployer recover the
/// exact application device without opening a stale COM endpoint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UsbResetInterface {
    pub instance_id: String,
    pub parent_instance_id: String,
    pub vid: u16,
    pub pid: u16,
    pub serial_number: String,
    /// WinUSB device-interface path for the fixed Pico SDK reset GUID.
    pub device_path: String,
    /// USB interface number carried by the composite `MI_xx` devnode.
    pub interface_number: u8,
    pub location_paths: Vec<String>,
}

#[cfg(any(windows, test))]
fn is_picotool_reset_compatible_id(value: &str) -> bool {
    value.eq_ignore_ascii_case("USB\\Class_ff&SubClass_00&Prot_01")
}

/// Best-effort enumeration of healthy Pico SDK application reset interfaces.
///
/// Windows exposes the function as the standard Raspberry Pi reset-interface
/// compatible ID. Other hosts currently return an empty list; their normal
/// libusb/picotool path remains unchanged.
pub fn present_usb_reset_interfaces() -> Vec<UsbResetInterface> {
    #[cfg(windows)]
    {
        imp::present_usb_reset_interfaces()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// Ask one exact Pico SDK WinUSB reset interface to enter BOOTSEL mode.
///
/// The interface must come from [`present_usb_reset_interfaces`], which binds
/// the live device path to its USB serial and VID/PID before this request is
/// issued. The board may disconnect before Windows reports completion; that
/// is the normal successful shape of the no-data control transfer, so the
/// deployer confirms success by waiting for the target BOOTSEL transport.
pub fn reset_usb_interface_to_bootsel(interface: &UsbResetInterface) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        imp::reset_usb_interface_to_bootsel(interface)
    }
    #[cfg(not(windows))]
    {
        let _ = interface;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "the native Pico reset interface is currently implemented only on Windows",
        ))
    }
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
/// equivalent
/// implemented yet (IOKit work is out of scope without a macOS host).
pub fn present_usb_problem_devices() -> Vec<UsbProblemDevice> {
    #[cfg(windows)]
    {
        imp::present_usb_problem_devices()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// Linux sibling of [`present_usb_problem_devices`]: `sysfs`-derived USB
/// devices with a concrete, observed fault (unauthorized, unconfigured, or a
/// CDC interface with no bound driver). Diagnostics only — never used to
/// drive `PortHealth` selection directly (that happens per-tty via
/// [`available_ports`]'s Linux enrichment).
#[cfg(target_os = "linux")]
pub fn present_usb_problem_devices_linux() -> Vec<crate::sysfs_usb::LinuxUsbProblemDevice> {
    crate::sysfs_usb::linux_usb_problem_devices_from_root(&crate::sysfs_usb::live_root())
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
            classify_port_health(PnpObservation::Present {
                status: 0,
                problem_code: 0,
            }),
            PortHealth::HealthyPresent
        );
        assert_eq!(
            classify_port_health(PnpObservation::Present {
                status: 0x1234,
                problem_code: 31,
            }),
            PortHealth::PresentProblem {
                problem_code: 31,
                status: Some(0x1234),
            }
        );
        assert_eq!(
            classify_port_health(PnpObservation::Phantom),
            PortHealth::Phantom {
                problem_code: None,
                status: None,
            }
        );
        assert_eq!(
            classify_port_health(PnpObservation::Unknown),
            PortHealth::Unknown
        );
        assert_eq!(
            health_for_endpoint(
                PnpObservation::Present {
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

#[cfg(windows)]
mod imp {
    use super::{
        DetectedPort, PnpObservation, UsbProblemDevice, UsbResetInterface, health_for_endpoint,
        is_picotool_reset_compatible_id,
    };
    use std::collections::{HashMap, HashSet};
    use std::io;
    use std::ptr;

    use serialport::{SerialPortInfo, SerialPortType, UsbPortInfo};
    use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
        CM_Get_DevNode_Status, CM_Get_Device_IDW, CM_Get_Parent, CR_NO_SUCH_DEVINST, CR_SUCCESS,
        DICS_FLAG_GLOBAL, DIGCF_ALLCLASSES, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, DIREG_DEV,
        HDEVINFO, MAX_DEVICE_ID_LEN, SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W,
        SP_DEVINFO_DATA, SPDRP_CLASS, SPDRP_COMPATIBLEIDS, SPDRP_FRIENDLYNAME, SPDRP_HARDWAREID,
        SPDRP_LOCATION_INFORMATION, SPDRP_MFG, SetupDiClassGuidsFromNameW,
        SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiEnumDeviceInterfaces,
        SetupDiGetClassDevsW, SetupDiGetDeviceInstanceIdW, SetupDiGetDeviceInterfaceDetailW,
        SetupDiGetDevicePropertyW, SetupDiGetDeviceRegistryPropertyW, SetupDiOpenDevRegKey,
    };
    use windows_sys::Win32::Devices::Properties::{
        DEVPKEY_Device_LocationPaths, DEVPROP_TYPE_STRING_LIST,
    };
    use windows_sys::Win32::Devices::Usb::{
        WINUSB_INTERFACE_HANDLE, WINUSB_SETUP_PACKET, WinUsb_ControlTransfer, WinUsb_Free,
        WinUsb_Initialize,
    };
    use windows_sys::Win32::Foundation::{
        CloseHandle, FALSE, FILETIME, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE, MAX_PATH,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ,
        FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_LOCAL_MACHINE, KEY_READ, REG_MULTI_SZ, REG_SZ, RegCloseKey, RegEnumValueW,
        RegOpenKeyExW, RegQueryInfoKeyW, RegQueryValueExW,
    };
    use windows_sys::core::GUID;

    const CONNECTOR_PUNCTUATION_SELECTION: &[char] = &[':', '_', '\u{ff3f}'];
    const PICO_RESET_INTERFACE_GUID: GUID = GUID::from_u128(0xbc7398c1_73cd_4cb7_98b8_913a8fca7bf6);
    const RESET_REQUEST_BOOTSEL: u8 = 0x01;
    // USB_DIR_OUT | USB_TYPE_CLASS | USB_RECIP_INTERFACE. This exactly
    // matches picotool's reset-interface request; the endpoint is vendor
    // class, but the control request itself is class-scoped.
    const RESET_REQUEST_TYPE: u8 = 0x21;

    fn as_utf16(utf8: &str) -> Vec<u16> {
        utf8.encode_utf16().chain(Some(0)).collect()
    }

    fn from_utf16_lossy_trimmed(utf16: &[u16]) -> String {
        String::from_utf16_lossy(utf16)
            .trim_end_matches(0 as char)
            .to_string()
    }

    fn get_ports_guids() -> serialport::Result<Vec<GUID>> {
        let class_names = ["Ports", "Modem"];
        let mut guids: Vec<GUID> = Vec::new();
        for class_name in class_names {
            let class_name_w = as_utf16(class_name);
            let mut num_guids: u32 = 1;
            let class_start_idx = guids.len();

            for _ in 0..2 {
                guids.resize(class_start_idx + num_guids as usize, GUID::from_u128(0));
                let guid_buffer = &mut guids[class_start_idx..];
                let res = unsafe {
                    SetupDiClassGuidsFromNameW(
                        class_name_w.as_ptr(),
                        guid_buffer.as_mut_ptr(),
                        guid_buffer.len() as u32,
                        &mut num_guids,
                    )
                };
                if res == FALSE {
                    return Err(serialport::Error::new(
                        serialport::ErrorKind::Unknown,
                        "Unable to determine number of Ports GUIDs",
                    ));
                }
                let len_cmp = guid_buffer.len().cmp(&(num_guids as usize));
                if len_cmp == std::cmp::Ordering::Less {
                    continue;
                } else if len_cmp == std::cmp::Ordering::Greater {
                    guids.truncate(class_start_idx + num_guids as usize);
                }
                break;
            }
        }
        Ok(guids)
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct HwidMatches<'hwid> {
        vid: &'hwid str,
        pid: &'hwid str,
        serial: Option<&'hwid str>,
        interface: Option<&'hwid str>,
    }

    impl<'hwid> HwidMatches<'hwid> {
        fn new(hwid: &'hwid str) -> Option<Self> {
            let mut hwid_tail = hwid;
            let vid_start = hwid.find("VID_")?;
            let vid = hwid_tail.get(vid_start + 4..vid_start + 8)?;
            hwid_tail = hwid_tail.get(vid_start + 8..)?;

            let pid = if hwid_tail.starts_with("&PID_") || hwid_tail.starts_with("+PID_") {
                hwid_tail.get(5..9)?
            } else {
                return None;
            };
            hwid_tail = hwid_tail.get(9..)?;

            let iid = if hwid_tail.starts_with("&MI_") || hwid_tail.starts_with("+MI_") {
                let iid = hwid_tail.get(4..6);
                hwid_tail = hwid_tail.get(6..).unwrap_or(hwid_tail);
                iid
            } else {
                None
            };

            let serial = if hwid_tail.starts_with('\\') || hwid_tail.starts_with('+') {
                hwid_tail.get(1..).and_then(|tail| {
                    let index = tail
                        .char_indices()
                        .find(|&(_, char)| {
                            !(char.is_alphanumeric()
                                || CONNECTOR_PUNCTUATION_SELECTION.contains(&char))
                        })
                        .map(|(index, _)| index)
                        .unwrap_or(tail.len());
                    tail.get(..index)
                })
            } else {
                None
            };

            Some(Self {
                vid,
                pid,
                serial,
                interface: iid,
            })
        }
    }

    /// Parse a Windows HWID string into [`UsbPortInfo`] (with the composite
    /// `MI_xx` interface index preserved). Pure — unit-tested below.
    ///
    /// VID/PID always come from the device's own hardware id (a composite
    /// interface's `MI_xx` hwid carries the same VID/PID as its parent). Only
    /// the serial number is taken from the parent for composite devices — and
    /// if the parent isn't available (a **phantom** devnode whose live parent
    /// no longer exists, i.e. the Status=Unknown Teensy case) we fall back to
    /// the child's own serial tail rather than giving up. This is the key
    /// difference from upstream serialport, which returns `None` (→ no VID/PID)
    /// for a composite devnode with no reachable parent. FastLED/fbuild#962.
    fn parse_usb_port_info(
        hardware_id: &str,
        parent_hardware_id: Option<&str>,
    ) -> Option<UsbPortInfo> {
        let child = HwidMatches::new(hardware_id)?;
        let interface = child.interface.and_then(|m| u8::from_str_radix(m, 16).ok());
        let serial = if interface.is_some() {
            parent_hardware_id
                .and_then(HwidMatches::new)
                .and_then(|p| p.serial)
                .or(child.serial)
        } else {
            child.serial
        };

        Some(UsbPortInfo {
            vid: u16::from_str_radix(child.vid, 16).ok()?,
            pid: u16::from_str_radix(child.pid, 16).ok()?,
            serial_number: serial.map(str::to_string),
            manufacturer: None,
            product: None,
            // The workspace enables serialport's `usbportinfo-interface`
            // feature (Cargo.toml) precisely so this field exists; it carries
            // the `MI_xx` index used to disambiguate Teensy Serial vs MIDI.
            interface,
        })
    }

    struct PortDevices {
        hdi: HDEVINFO,
        dev_idx: u32,
    }

    impl PortDevices {
        fn new(guid: &GUID) -> Self {
            PortDevices {
                // flags = 0 (NOT `DIGCF_PRESENT`) so non-present / phantom /
                // Status=Unknown devnodes — every PJRC/Teensy composite serial
                // port — are enumerated too. We re-derive real presence below
                // via `CM_Get_DevNode_Status`. FastLED/fbuild#962.
                hdi: unsafe { SetupDiGetClassDevsW(guid, ptr::null(), 0, 0) },
                dev_idx: 0,
            }
        }
    }

    impl Iterator for PortDevices {
        type Item = PortDevice;

        fn next(&mut self) -> Option<PortDevice> {
            let mut port_dev = PortDevice {
                hdi: self.hdi,
                devinfo_data: SP_DEVINFO_DATA {
                    cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32,
                    ClassGuid: GUID::from_u128(0),
                    DevInst: 0,
                    Reserved: 0,
                },
            };
            let res = unsafe {
                SetupDiEnumDeviceInfo(self.hdi, self.dev_idx, &mut port_dev.devinfo_data)
            };
            if res == FALSE {
                None
            } else {
                self.dev_idx += 1;
                Some(port_dev)
            }
        }
    }

    impl Drop for PortDevices {
        fn drop(&mut self) {
            unsafe {
                SetupDiDestroyDeviceInfoList(self.hdi);
            }
        }
    }

    struct PortDevice {
        hdi: HDEVINFO,
        devinfo_data: SP_DEVINFO_DATA,
    }

    impl PortDevice {
        fn parent_instance_id(&mut self) -> Option<String> {
            let mut result_buf = [0u16; MAX_PATH as usize];
            let mut parent_device_instance_id = 0;
            let res = unsafe {
                CM_Get_Parent(&mut parent_device_instance_id, self.devinfo_data.DevInst, 0)
            };
            if res == CR_SUCCESS {
                let buffer_len = result_buf.len() - 1;
                let res = unsafe {
                    CM_Get_Device_IDW(
                        parent_device_instance_id,
                        result_buf.as_mut_ptr(),
                        buffer_len as u32,
                        0,
                    )
                };
                if res == CR_SUCCESS {
                    Some(from_utf16_lossy_trimmed(&result_buf))
                } else {
                    None
                }
            } else {
                None
            }
        }

        fn instance_id(&mut self) -> Option<String> {
            let mut result_buf = [0u16; MAX_DEVICE_ID_LEN as usize];
            let working_buffer_len = result_buf.len() - 1;
            let mut desired_result_len = 0;
            let res = unsafe {
                SetupDiGetDeviceInstanceIdW(
                    self.hdi,
                    &self.devinfo_data,
                    result_buf.as_mut_ptr(),
                    working_buffer_len as u32,
                    &mut desired_result_len,
                )
            };
            if res == FALSE {
                self.property(SPDRP_HARDWAREID)
            } else {
                let actual_result_len = working_buffer_len.min(desired_result_len as usize);
                Some(from_utf16_lossy_trimmed(&result_buf[..actual_result_len]))
            }
        }

        // Retrieves the port name (i.e. COM6) associated with this device.
        fn name(&mut self) -> String {
            let hkey = unsafe {
                SetupDiOpenDevRegKey(
                    self.hdi,
                    &self.devinfo_data,
                    DICS_FLAG_GLOBAL,
                    0,
                    DIREG_DEV,
                    KEY_READ,
                )
            };
            if hkey == INVALID_HANDLE_VALUE {
                return String::new();
            }

            let mut port_name_buffer = [0u16; MAX_PATH as usize];
            let buffer_byte_len = 2 * port_name_buffer.len() as u32;
            let mut byte_len = buffer_byte_len;
            let mut value_type = 0;
            let value_name = as_utf16("PortName");
            let err = unsafe {
                RegQueryValueExW(
                    hkey,
                    value_name.as_ptr(),
                    ptr::null_mut(),
                    &mut value_type,
                    port_name_buffer.as_mut_ptr() as *mut u8,
                    &mut byte_len,
                )
            };
            unsafe { RegCloseKey(hkey) };
            if err != 0 {
                return String::new();
            }
            if value_type != REG_SZ || byte_len % 2 != 0 || byte_len > buffer_byte_len {
                return String::new();
            }
            let len = buffer_byte_len as usize / 2;
            let port_name = &port_name_buffer[0..len];
            from_utf16_lossy_trimmed(port_name)
        }

        /// Read the Config Manager observation without flattening its three
        /// important outcomes.  A missing live devnode is a phantom; a query
        /// failure that is not that explicit state remains unknown.
        fn pnp_observation(&mut self) -> PnpObservation {
            let mut status = 0u32;
            let mut problem = 0u32;
            // SAFETY: `DevInst` comes from the live SetupAPI record and both
            // output pointers reference initialized writable local storage.
            let res = unsafe {
                CM_Get_DevNode_Status(&mut status, &mut problem, self.devinfo_data.DevInst, 0)
            };
            if res == CR_SUCCESS {
                PnpObservation::Present {
                    status,
                    problem_code: problem,
                }
            } else if res == CR_NO_SUCH_DEVINST {
                PnpObservation::Phantom
            } else {
                PnpObservation::Unknown
            }
        }

        fn port_type(
            &mut self,
            instance_id: Option<&str>,
            parent_instance_id: Option<&str>,
        ) -> SerialPortType {
            instance_id
                .and_then(|id| parse_usb_port_info(id, parent_instance_id))
                .map(|mut info: UsbPortInfo| {
                    info.manufacturer = self.property(SPDRP_MFG);
                    info.product = self.property(SPDRP_FRIENDLYNAME);
                    SerialPortType::UsbPort(info)
                })
                .unwrap_or(SerialPortType::Unknown)
        }

        fn property(&mut self, property_id: u32) -> Option<String> {
            let mut value_type = 0;
            let mut property_buf = [0u16; MAX_PATH as usize];
            let res = unsafe {
                SetupDiGetDeviceRegistryPropertyW(
                    self.hdi,
                    &self.devinfo_data,
                    property_id,
                    &mut value_type,
                    property_buf.as_mut_ptr() as *mut u8,
                    property_buf.len() as u32,
                    ptr::null_mut(),
                )
            };
            if res == FALSE || value_type != REG_SZ {
                return None;
            }
            from_utf16_lossy_trimmed(&property_buf)
                .split(';')
                .next_back()
                .map(str::to_string)
        }
    }

    fn ancestor_ids(devinst: u32) -> Vec<String> {
        let mut ids = Vec::new();
        let mut current = devinst;
        for _ in 0..16 {
            let mut parent = 0;
            let result = unsafe { CM_Get_Parent(&mut parent, current, 0) };
            if result != CR_SUCCESS {
                break;
            }
            let mut buffer = [0u16; MAX_DEVICE_ID_LEN as usize];
            let result =
                unsafe { CM_Get_Device_IDW(parent, buffer.as_mut_ptr(), buffer.len() as u32, 0) };
            if result != CR_SUCCESS {
                break;
            }
            let length = buffer
                .iter()
                .position(|&unit| unit == 0)
                .unwrap_or(buffer.len());
            ids.push(String::from_utf16_lossy(&buffer[..length]));
            current = parent;
        }
        ids
    }

    fn classify_usb_ancestry(devinst: u32) -> Option<bool> {
        let ancestors = ancestor_ids(devinst);
        let root_index = ancestors
            .iter()
            .position(|id| id.to_ascii_uppercase().starts_with("USB\\ROOT_HUB"))?;
        Some(ancestors[..root_index].iter().any(|id| {
            let upper = id.to_ascii_uppercase();
            upper.starts_with("USB\\VID_") && upper.contains("&PID_")
        }))
    }

    fn location_paths_from_info(hdi: HDEVINFO, info: &SP_DEVINFO_DATA) -> Vec<String> {
        let mut property_type = 0u32;
        let mut required_bytes = 0u32;
        // First call obtains the required byte count. SetupAPI reports
        // insufficient buffer here, so the return value itself is not the
        // success signal; a non-zero required size is.
        unsafe {
            SetupDiGetDevicePropertyW(
                hdi,
                info,
                &DEVPKEY_Device_LocationPaths,
                &mut property_type,
                std::ptr::null_mut(),
                0,
                &mut required_bytes,
                0,
            )
        };
        if required_bytes < 2 {
            return Vec::new();
        }
        let mut buffer = vec![0u16; (required_bytes as usize).div_ceil(2)];
        let ok = unsafe {
            SetupDiGetDevicePropertyW(
                hdi,
                info,
                &DEVPKEY_Device_LocationPaths,
                &mut property_type,
                buffer.as_mut_ptr().cast(),
                required_bytes,
                &mut required_bytes,
                0,
            )
        };
        if ok == FALSE || property_type != DEVPROP_TYPE_STRING_LIST {
            return Vec::new();
        }
        buffer
            .split(|unit| *unit == 0)
            .take_while(|segment| !segment.is_empty())
            .map(String::from_utf16_lossy)
            .filter(|path| !path.is_empty())
            .collect()
    }

    pub(super) fn present_usb_problem_devices() -> Vec<UsbProblemDevice> {
        // Enumerate by the `USB` *enumerator* with DIGCF_ALLCLASSES, not by
        // the USB *setup class*: a driverless devnode (e.g. a BOOTSEL
        // PICOBOOT interface stuck at CM_PROB_FAILED_INSTALL) has no setup
        // class at all and is invisible to a class-scoped query, which hid
        // exactly the problem interface the FastLED/fbuild#1152 recovery
        // request needs to target.
        let enumerator: Vec<u16> = "USB".encode_utf16().chain(Some(0)).collect();
        let hdi = unsafe {
            SetupDiGetClassDevsW(
                std::ptr::null(),
                enumerator.as_ptr(),
                0,
                DIGCF_PRESENT | DIGCF_ALLCLASSES,
            )
        };
        if hdi == INVALID_HANDLE_VALUE {
            return Vec::new();
        }

        let mut devices = Vec::new();
        let mut index = 0u32;
        loop {
            let mut info = SP_DEVINFO_DATA {
                cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32,
                ClassGuid: GUID::from_u128(0),
                DevInst: 0,
                Reserved: 0,
            };
            if unsafe { SetupDiEnumDeviceInfo(hdi, index, &mut info) } == FALSE {
                break;
            }
            index += 1;

            let Some(instance_id) = device_instance_id_from_info(hdi, &info) else {
                continue;
            };
            if !instance_id.to_ascii_uppercase().starts_with("USB\\") {
                continue;
            }
            let mut status = 0u32;
            let mut problem_code = 0u32;
            if unsafe { CM_Get_DevNode_Status(&mut status, &mut problem_code, info.DevInst, 0) }
                != CR_SUCCESS
                || problem_code == 0
            {
                continue;
            }

            devices.push(UsbProblemDevice {
                instance_id,
                problem_code,
                friendly_name: property_from_info(hdi, &info, SPDRP_FRIENDLYNAME),
                location: property_from_info(hdi, &info, SPDRP_LOCATION_INFORMATION),
                behind_external_hub: classify_usb_ancestry(info.DevInst),
                parent_instance_id: ancestor_ids(info.DevInst).into_iter().next(),
                device_class: property_from_info(hdi, &info, SPDRP_CLASS),
                location_paths: location_paths_from_info(hdi, &info),
            });
        }
        unsafe {
            SetupDiDestroyDeviceInfoList(hdi);
        }
        devices
    }

    pub(super) fn present_usb_reset_interfaces() -> Vec<UsbResetInterface> {
        let device_paths = pico_reset_interface_paths();
        let enumerator: Vec<u16> = "USB".encode_utf16().chain(Some(0)).collect();
        let hdi = unsafe {
            SetupDiGetClassDevsW(
                std::ptr::null(),
                enumerator.as_ptr(),
                0,
                DIGCF_PRESENT | DIGCF_ALLCLASSES,
            )
        };
        if hdi == INVALID_HANDLE_VALUE {
            return Vec::new();
        }

        let mut devices = Vec::new();
        let mut index = 0u32;
        loop {
            let mut info = SP_DEVINFO_DATA {
                cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32,
                ClassGuid: GUID::from_u128(0),
                DevInst: 0,
                Reserved: 0,
            };
            if unsafe { SetupDiEnumDeviceInfo(hdi, index, &mut info) } == FALSE {
                break;
            }
            index += 1;

            let compatible_ids = string_list_property_from_info(hdi, &info, SPDRP_COMPATIBLEIDS);
            if !compatible_ids
                .iter()
                .any(|value| is_picotool_reset_compatible_id(value))
            {
                continue;
            }
            let mut status = 0u32;
            let mut problem_code = 0u32;
            if unsafe { CM_Get_DevNode_Status(&mut status, &mut problem_code, info.DevInst, 0) }
                != CR_SUCCESS
                || problem_code != 0
            {
                continue;
            }

            let Some(instance_id) = device_instance_id_from_info(hdi, &info) else {
                continue;
            };
            let Some(parent_instance_id) = ancestor_ids(info.DevInst).into_iter().next() else {
                continue;
            };
            let Some(identity) = parse_usb_port_info(&instance_id, Some(&parent_instance_id))
            else {
                continue;
            };
            let Some(serial_number) = identity.serial_number else {
                continue;
            };
            let Some(interface_number) = identity.interface else {
                continue;
            };
            let Some(device_path) = device_paths.get(&instance_id.to_ascii_uppercase()) else {
                continue;
            };
            devices.push(UsbResetInterface {
                instance_id,
                parent_instance_id,
                vid: identity.vid,
                pid: identity.pid,
                serial_number,
                device_path: device_path.clone(),
                interface_number,
                location_paths: location_paths_from_info(hdi, &info),
            });
        }
        unsafe {
            SetupDiDestroyDeviceInfoList(hdi);
        }
        devices.sort_by(|left, right| left.instance_id.cmp(&right.instance_id));
        devices
    }

    fn pico_reset_interface_paths() -> HashMap<String, String> {
        let hdi = unsafe {
            SetupDiGetClassDevsW(
                &PICO_RESET_INTERFACE_GUID,
                std::ptr::null(),
                0,
                DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
            )
        };
        if hdi == INVALID_HANDLE_VALUE {
            return HashMap::new();
        }

        let mut paths = HashMap::new();
        let mut index = 0u32;
        loop {
            let mut interface = SP_DEVICE_INTERFACE_DATA {
                cbSize: std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
                InterfaceClassGuid: GUID::from_u128(0),
                Flags: 0,
                Reserved: 0,
            };
            if unsafe {
                SetupDiEnumDeviceInterfaces(
                    hdi,
                    std::ptr::null(),
                    &PICO_RESET_INTERFACE_GUID,
                    index,
                    &mut interface,
                )
            } == FALSE
            {
                break;
            }
            index += 1;

            let mut required_bytes = 0u32;
            unsafe {
                SetupDiGetDeviceInterfaceDetailW(
                    hdi,
                    &interface,
                    std::ptr::null_mut(),
                    0,
                    &mut required_bytes,
                    std::ptr::null_mut(),
                )
            };
            if required_bytes < std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32 {
                continue;
            }

            // `Vec<usize>` provides pointer alignment suitable for the
            // variable-sized SetupAPI detail record while still letting the
            // API state its required byte count exactly.
            let units = (required_bytes as usize).div_ceil(std::mem::size_of::<usize>());
            let mut storage = vec![0usize; units];
            let detail = storage
                .as_mut_ptr()
                .cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
            unsafe {
                (*detail).cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            }
            let mut info = SP_DEVINFO_DATA {
                cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32,
                ClassGuid: GUID::from_u128(0),
                DevInst: 0,
                Reserved: 0,
            };
            if unsafe {
                SetupDiGetDeviceInterfaceDetailW(
                    hdi,
                    &interface,
                    detail,
                    required_bytes,
                    &mut required_bytes,
                    &mut info,
                )
            } == FALSE
            {
                continue;
            }
            let Some(instance_id) = device_instance_id_from_info(hdi, &info) else {
                continue;
            };
            let path_ptr = unsafe { std::ptr::addr_of!((*detail).DevicePath).cast::<u16>() };
            let path_offset = std::mem::offset_of!(SP_DEVICE_INTERFACE_DETAIL_DATA_W, DevicePath);
            let max_units = (required_bytes as usize).saturating_sub(path_offset) / 2;
            let path_units = unsafe { std::slice::from_raw_parts(path_ptr, max_units) };
            let length = path_units
                .iter()
                .position(|unit| *unit == 0)
                .unwrap_or(path_units.len());
            if length != 0 {
                paths.insert(
                    instance_id.to_ascii_uppercase(),
                    String::from_utf16_lossy(&path_units[..length]),
                );
            }
        }
        unsafe {
            SetupDiDestroyDeviceInfoList(hdi);
        }
        paths
    }

    pub(super) fn reset_usb_interface_to_bootsel(interface: &UsbResetInterface) -> io::Result<()> {
        let path = as_utf16(&interface.device_path);
        let device = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED,
                0,
            )
        };
        if device == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }

        let mut winusb: WINUSB_INTERFACE_HANDLE = 0;
        if unsafe { WinUsb_Initialize(device, &mut winusb) } == FALSE {
            let error = io::Error::last_os_error();
            unsafe {
                CloseHandle(device);
            }
            return Err(error);
        }

        let setup = WINUSB_SETUP_PACKET {
            RequestType: RESET_REQUEST_TYPE,
            Request: RESET_REQUEST_BOOTSEL,
            Value: 0,
            Index: u16::from(interface.interface_number),
            Length: 0,
        };
        let mut transferred = 0u32;
        let transfer_ok = unsafe {
            WinUsb_ControlTransfer(
                winusb,
                setup,
                std::ptr::null_mut(),
                0,
                &mut transferred,
                std::ptr::null(),
            )
        };
        let transfer_error = (transfer_ok == FALSE).then(io::Error::last_os_error);
        unsafe {
            WinUsb_Free(winusb);
            CloseHandle(device);
        }
        if let Some(error) = transfer_error {
            // The reset handler does not return. Windows can therefore report
            // the expected disconnect as a failed zero-length transfer even
            // though the request was accepted. The deployer performs the
            // authoritative BOOTSEL wait immediately after this call.
            tracing::debug!(
                instance_id = %interface.instance_id,
                %error,
                "Pico reset interface disconnected while handling the BOOTSEL request"
            );
        }
        Ok(())
    }

    fn device_instance_id_from_info(hdi: HDEVINFO, info: &SP_DEVINFO_DATA) -> Option<String> {
        let mut buffer = [0u16; MAX_DEVICE_ID_LEN as usize];
        let mut required = 0u32;
        let ok = unsafe {
            SetupDiGetDeviceInstanceIdW(
                hdi,
                info,
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                &mut required,
            )
        };
        if ok == FALSE {
            return None;
        }
        let length = buffer
            .iter()
            .position(|&unit| unit == 0)
            .unwrap_or(buffer.len());
        Some(String::from_utf16_lossy(&buffer[..length]))
    }

    fn property_from_info(
        hdi: HDEVINFO,
        info: &SP_DEVINFO_DATA,
        property_id: u32,
    ) -> Option<String> {
        let mut value_type = 0u32;
        let mut buffer = [0u16; MAX_PATH as usize];
        let ok = unsafe {
            SetupDiGetDeviceRegistryPropertyW(
                hdi,
                info,
                property_id,
                &mut value_type,
                buffer.as_mut_ptr() as *mut u8,
                (buffer.len() * 2) as u32,
                std::ptr::null_mut(),
            )
        };
        if ok == FALSE || value_type != REG_SZ {
            return None;
        }
        let length = buffer
            .iter()
            .position(|&unit| unit == 0)
            .unwrap_or(buffer.len());
        let value = String::from_utf16_lossy(&buffer[..length]);
        (!value.is_empty()).then_some(value)
    }

    fn string_list_property_from_info(
        hdi: HDEVINFO,
        info: &SP_DEVINFO_DATA,
        property_id: u32,
    ) -> Vec<String> {
        let mut value_type = 0u32;
        let mut required_bytes = 0u32;
        unsafe {
            SetupDiGetDeviceRegistryPropertyW(
                hdi,
                info,
                property_id,
                &mut value_type,
                std::ptr::null_mut(),
                0,
                &mut required_bytes,
            )
        };
        if required_bytes < 2 {
            return Vec::new();
        }
        let mut buffer = vec![0u16; (required_bytes as usize).div_ceil(2)];
        let ok = unsafe {
            SetupDiGetDeviceRegistryPropertyW(
                hdi,
                info,
                property_id,
                &mut value_type,
                buffer.as_mut_ptr().cast(),
                required_bytes,
                &mut required_bytes,
            )
        };
        if ok == FALSE || value_type != REG_MULTI_SZ {
            return Vec::new();
        }
        buffer
            .split(|unit| *unit == 0)
            .take_while(|segment| !segment.is_empty())
            .map(String::from_utf16_lossy)
            .filter(|value| !value.is_empty())
            .collect()
    }

    /// COM ports listed under `HKLM\HARDWARE\DEVICEMAP\SERIALCOMM` that the
    /// "Ports" class walk did not surface (parity with upstream serialport).
    fn get_registry_com_ports() -> HashSet<String> {
        let mut ports_list = HashSet::new();
        let reg_key = as_utf16("HARDWARE\\DEVICEMAP\\SERIALCOMM");
        let mut ports_key: HKEY = 0;
        let open_res = unsafe {
            RegOpenKeyExW(
                HKEY_LOCAL_MACHINE,
                reg_key.as_ptr(),
                0,
                KEY_READ,
                &mut ports_key,
            )
        };
        if open_res != 0 {
            return ports_list;
        }
        let mut class_name_buff = [0u16; MAX_PATH as usize];
        let mut class_name_size = MAX_PATH;
        let mut sub_key_count = 0;
        let mut largest_sub_key = 0;
        let mut largest_class_string = 0;
        let mut num_key_values = 0;
        let mut longest_value_name = 0;
        let mut longest_value_data = 0;
        let mut size_security_desc = 0;
        let mut last_write_time = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let query_res = unsafe {
            RegQueryInfoKeyW(
                ports_key,
                class_name_buff.as_mut_ptr(),
                &mut class_name_size,
                ptr::null(),
                &mut sub_key_count,
                &mut largest_sub_key,
                &mut largest_class_string,
                &mut num_key_values,
                &mut longest_value_name,
                &mut longest_value_data,
                &mut size_security_desc,
                &mut last_write_time,
            )
        };
        if query_res == 0 {
            for idx in 0..num_key_values {
                let mut val_name_buff = [0u16; MAX_PATH as usize];
                let mut val_name_size = MAX_PATH;
                let mut value_type = 0;
                let mut val_data = [0u16; MAX_PATH as usize];
                let buffer_byte_len = 2 * val_data.len() as u32;
                let mut byte_len = buffer_byte_len;
                let res = unsafe {
                    RegEnumValueW(
                        ports_key,
                        idx,
                        val_name_buff.as_mut_ptr(),
                        &mut val_name_size,
                        ptr::null(),
                        &mut value_type,
                        val_data.as_mut_ptr() as *mut u8,
                        &mut byte_len,
                    )
                };
                if res != 0
                    || value_type != REG_SZ
                    || byte_len % 2 != 0
                    || byte_len > buffer_byte_len
                {
                    break;
                }
                let val_data = from_utf16_lossy_trimmed(unsafe {
                    let utf16_len = byte_len / 2;
                    std::slice::from_raw_parts(val_data.as_ptr(), utf16_len as usize)
                });
                ports_list.insert(val_data);
            }
        }
        unsafe { RegCloseKey(ports_key) };
        ports_list
    }

    pub(super) fn available_ports() -> serialport::Result<Vec<DetectedPort>> {
        let mut ports = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for guid in get_ports_guids()? {
            let port_devices = PortDevices::new(&guid);
            for mut port_device in port_devices {
                let port_name = port_device.name();
                if port_name.is_empty() {
                    // No PortName in the devnode registry key → not an actual
                    // COM port (e.g. a modem enumerator entry). Skip.
                    continue;
                }
                if port_name.starts_with("LPT") {
                    continue;
                }
                let instance_id = port_device.instance_id();
                let parent_instance_id = port_device.parent_instance_id();
                let ancestor_instance_ids = ancestor_ids(port_device.devinfo_data.DevInst);
                let pnp_observation = port_device.pnp_observation();
                let port_type =
                    port_device.port_type(instance_id.as_deref(), parent_instance_id.as_deref());
                let is_usb = matches!(port_type, SerialPortType::UsbPort(_));
                // Include every present port (unchanged behaviour), PLUS
                // non-present USB serial ports — the Status=Unknown Teensy
                // case the whole fix exists for. A non-present *non-USB*
                // devnode is a stale phantom with no VID:PID to act on, so we
                // leave it out to avoid resurrecting ancient ACPI/BT junk.
                // FastLED/fbuild#962.
                if matches!(pnp_observation, super::PnpObservation::Phantom) && !is_usb {
                    continue;
                }
                let health = health_for_endpoint(pnp_observation, is_usb);
                // A phantom devnode can be enumerated once per matching class
                // GUID; de-dup on the COM name.
                if !seen.insert(port_name.clone()) {
                    continue;
                }
                ports.push(DetectedPort {
                    info: SerialPortInfo {
                        port_name,
                        port_type,
                    },
                    health,
                    instance_id,
                    parent_instance_id,
                    ancestor_instance_ids,
                    location_paths: location_paths_from_info(
                        port_device.hdi,
                        &port_device.devinfo_data,
                    ),
                });
            }
        }

        // Fold in any DEVICEMAP\SERIALCOMM ports not already found.
        for raw_port in get_registry_com_ports() {
            if seen.insert(raw_port.clone()) {
                ports.push(DetectedPort::unknown(SerialPortInfo {
                    port_name: raw_port,
                    port_type: SerialPortType::Unknown,
                }));
            }
        }
        Ok(ports)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parses_teensy_composite_serial_with_interface() {
            // Teensy 4.x USB Serial enumerates as a composite MI_00 interface;
            // the serial comes from the PARENT instance id, VID/PID from the child.
            let child = r"USB\VID_16C0&PID_0483&MI_00\8&226AD2B7&0&0000";
            let parent = r"USB\VID_16C0&PID_0483\12345678";
            let info = parse_usb_port_info(child, Some(parent)).expect("parse");
            assert_eq!(info.vid, 0x16C0);
            assert_eq!(info.pid, 0x0483);
            assert_eq!(info.serial_number.as_deref(), Some("12345678"));
            assert_eq!(info.interface, Some(0));
        }

        #[test]
        fn parses_phantom_teensy_composite_without_parent() {
            // The bug's core case: a Status=Unknown Teensy port is a phantom
            // devnode whose live parent no longer exists, so no parent hwid is
            // available. We must STILL recover VID/PID (16C0:0483) from the
            // child's own MI_00 hardware id — upstream serialport returns None
            // here, which is why the Teensy was invisible. FastLED/fbuild#962.
            let child = r"USB\VID_16C0&PID_0483&MI_00\8&226AD2B7&0&0000";
            let info = parse_usb_port_info(child, None).expect("parse without parent");
            assert_eq!(info.vid, 0x16C0);
            assert_eq!(info.pid, 0x0483);
            assert_eq!(info.interface, Some(0));
        }

        #[test]
        fn parses_teensy_serial_midi_audio_pid() {
            let child = r"USB\VID_16C0&PID_0489&MI_00\9&32144BF9&0&0000";
            let parent = r"USB\VID_16C0&PID_0489\ABCDEF";
            let info = parse_usb_port_info(child, Some(parent)).expect("parse");
            assert_eq!(info.vid, 0x16C0);
            assert_eq!(info.pid, 0x0489);
            assert_eq!(info.interface, Some(0));
        }

        #[test]
        fn non_composite_device_has_no_interface() {
            let info = parse_usb_port_info(r"USB\VID_303A&PID_1001\B4:3A:45:B0:08:24", None)
                .expect("parse");
            assert_eq!(info.vid, 0x303A);
            assert_eq!(info.interface, None);
            assert_eq!(info.serial_number.as_deref(), Some("B4:3A:45:B0:08:24"));
        }
    }
}
