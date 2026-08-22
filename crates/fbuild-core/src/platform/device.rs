//! Neutral serial-port, USB PnP, and USB-recovery mechanics.
//!
//! Every native enumeration surface lives behind this module: the Windows
//! SetupAPI/CfgMgr32/WinUsb fork (`selected::device` for enumeration and
//! `selected::usb_pnp` for PnP/recovery) and the portable `serialport`
//! delegate on Unix hosts. Callers receive host-neutral facts
//! ([`SerialPortFacts`]) plus the raw [`DevNodeObservation`] each host can
//! honestly provide, and keep all selection policy (which unhealthy endpoints
//! are deployable, which are diagnostics-only) on their side of the seam.

use std::time::Duration;

/// Raw host observation of a serial devnode, before any policy flattening.
///
/// Mirrors what `CM_Get_DevNode_Status` can say about a devnode. Hosts
/// without an equivalent signal report [`DevNodeObservation::Unknown`] and
/// callers keep their cross-platform default behavior.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DevNodeObservation {
    /// The devnode is in the live tree; the host reported its status word and
    /// problem code (`0` means healthy).
    Present { status: u32, problem_code: u32 },
    /// The devnode is retained by host history but is not in the live tree.
    Phantom,
    /// The host cannot provide equivalent data.
    Unknown,
}

/// Kernel-driver classification of a serial devnode (FastLED/fbuild#895).
///
/// Returned only when the host can confidently classify; ambiguous cases
/// yield `None` so callers fall back to their existing defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KernelDriverClass {
    /// The kernel created this port via its CDC-ACM stack (Linux
    /// `cdc_acm.ko`, macOS IOUSBHostFamily CDC, Windows `usbser` once
    /// implemented).
    CdcAcm,
    /// The kernel created this port via a chip-specific USB-serial bridge
    /// driver (Linux `ftdi_sio`/`cp210x`/`ch341`/..., macOS vendor drivers,
    /// Windows `FTDIBUS`/`silabser`/... once implemented).
    UsbSerialBridge,
}

/// USB identity facts for a serial endpoint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UsbSerialIdentityFacts {
    pub vid: u16,
    pub pid: u16,
    pub serial_number: Option<String>,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    /// Composite-function index (`MI_xx`) when the endpoint is one function
    /// of a composite device; used to disambiguate e.g. Teensy Serial vs MIDI.
    pub interface: Option<u8>,
}

/// Host-neutral view of one serial endpoint's kind. Only shapes some host
/// actually reports today; PCI/Bluetooth endpoints carry no identity facts
/// in either backend and land in [`SerialPortTypeFacts::Unknown`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SerialPortTypeFacts {
    Usb(UsbSerialIdentityFacts),
    Unknown,
}

/// A serial endpoint plus the raw host facts needed to decide whether it is
/// safe to select. This is the neutral counterpart of the caller-facing
/// record; health *policy* (flattening [`DevNodeObservation`] into a
/// deployable/not-deployable verdict) stays with the caller.
#[derive(Clone, Debug)]
pub struct SerialPortFacts {
    pub port_name: String,
    pub port_type: SerialPortTypeFacts,
    pub observation: DevNodeObservation,
    /// Canonical Plug and Play device instance ID when the host exposes one.
    pub instance_id: Option<String>,
    /// Immediate parent device instance ID when the host exposes one.
    pub parent_instance_id: Option<String>,
    /// Full USB ancestor chain, nearest first, when the host exposes one.
    pub ancestor_instance_ids: Vec<String>,
    /// Physical USB location paths. Empty when unavailable. These are
    /// identity history only and never make a phantom endpoint selectable.
    pub location_paths: Vec<String>,
}

/// Map a portable `serialport` enumeration entry onto neutral facts.
///
/// Used by the Unix delegates, which have no richer source than the
/// portable library; `observation` starts at [`DevNodeObservation::Unknown`]
/// and enrichment happens above the seam. Public so callers holding a
/// `SerialPortInfo` obtained elsewhere can use the same mapping.
pub fn facts_from_port_info(info: serialport::SerialPortInfo) -> SerialPortFacts {
    let port_type = match info.port_type {
        serialport::SerialPortType::UsbPort(usb) => {
            SerialPortTypeFacts::Usb(UsbSerialIdentityFacts {
                vid: usb.vid,
                pid: usb.pid,
                serial_number: usb.serial_number,
                manufacturer: usb.manufacturer,
                product: usb.product,
                interface: usb.interface,
            })
        }
        // PCI/Bluetooth are unit variants upstream — no facts to carry.
        serialport::SerialPortType::PciPort
        | serialport::SerialPortType::BluetoothPort
        | serialport::SerialPortType::Unknown => SerialPortTypeFacts::Unknown,
    };
    SerialPortFacts {
        port_name: info.port_name,
        port_type,
        observation: DevNodeObservation::Unknown,
        instance_id: None,
        parent_instance_id: None,
        ancestor_instance_ids: Vec::new(),
        location_paths: Vec::new(),
    }
}

/// Enumerate every serial port currently visible to the OS, including
/// endpoints whose devnode reports a non-OK problem status where the host
/// supports that (Windows; FastLED/fbuild#962).
pub fn available_serial_ports() -> std::io::Result<Vec<SerialPortFacts>> {
    super::selected::device::available_serial_ports()
}

/// Detect which kernel driver class instantiated a serial devnode.
///
/// Returns `None` when the host cannot classify (unsupported platform,
/// disconnected port, container without sysfs, ambiguous name). Purely
/// additive: callers MUST keep their existing fallback on `None`.
pub fn detect_serial_kernel_driver(port_name: &str) -> Option<KernelDriverClass> {
    super::selected::device::detect_serial_kernel_driver(port_name)
}

/// Live sysfs USB topology root (`/sys/bus/usb/devices`-shaped) when the
/// host provides one, `None` elsewhere.
pub fn live_sysfs_usb_root() -> Option<crate::path::NormalizedPath> {
    super::selected::device::live_sysfs_usb_root()
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
    /// Needed to compose an exact-device USB recovery request for a problem
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

/// Best-effort enumeration of present USB devnodes with a non-zero problem
/// code. Empty on hosts without an equivalent diagnostic, and never makes a
/// port scan fail merely because host diagnostics are unavailable.
pub fn present_usb_problem_devices() -> Vec<UsbProblemDevice> {
    super::selected::usb_pnp::present_usb_problem_devices()
}

/// Best-effort enumeration of healthy Pico SDK application reset interfaces.
/// Empty on hosts without the WinUSB reset surface; their normal
/// libusb/picotool path remains unchanged.
pub fn present_usb_reset_interfaces() -> Vec<UsbResetInterface> {
    super::selected::usb_pnp::present_usb_reset_interfaces()
}

/// Ask one exact Pico SDK WinUSB reset interface to enter BOOTSEL mode.
///
/// The interface must come from [`present_usb_reset_interfaces`], which binds
/// the live device path to its USB serial and VID/PID before this request is
/// issued. The board may disconnect before the OS reports completion; that
/// is the normal successful shape of the no-data control transfer, so the
/// deployer confirms success by waiting for the target BOOTSEL transport.
pub fn reset_usb_interface_to_bootsel(interface: &UsbResetInterface) -> std::io::Result<()> {
    super::selected::usb_pnp::reset_usb_interface_to_bootsel(interface)
}

/// A PnP devnode observed directly by the USB recovery backend.
///
/// Facts only: the recovery ladder revalidates every field against the
/// caller's request before any operation is allowed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UsbPnpDevice {
    pub instance_id: String,
    pub parent_instance_id: Option<String>,
    pub device_class: String,
    pub vid: u16,
    pub pid: u16,
    pub serial: Option<String>,
    pub health: crate::usb::UsbRecoveryHealth,
    pub location_paths: Vec<String>,
}

/// Return the current node. `allow_phantom` is required only to inspect the
/// original recovery target; verified parents are always looked up live.
pub fn inspect_usb_pnp_device(
    instance_id: &str,
    allow_phantom: bool,
) -> Result<UsbPnpDevice, String> {
    super::selected::usb_pnp::inspect_usb_pnp_device(instance_id, allow_phantom)
}

/// Re-enumerate only the exact, verified live parent of a phantom target.
pub fn reenumerate_usb_parent(parent_instance_id: &str) -> Result<(), String> {
    super::selected::usb_pnp::reenumerate_usb_parent(parent_instance_id)
}

/// Restart only the exact, verified present target child (or the equally
/// verified healthy parent composite of a problematic interface devnode;
/// FastLED/fbuild#1152). Never call this with an unverified instance ID.
pub fn restart_usb_device(instance_id: &str) -> Result<(), String> {
    super::selected::usb_pnp::restart_usb_device(instance_id)
}

/// Whether a Windows compatible-ID string is the standard Raspberry Pi
/// Pico SDK application-mode reset interface (`USB\Class_ff&SubClass_00&Prot_01`).
pub fn is_picotool_reset_compatible_id(value: &str) -> bool {
    value.eq_ignore_ascii_case("USB\\Class_ff&SubClass_00&Prot_01")
}

/// Number of bounded post-operation observations the host backend wants.
/// Fakes stay instant; real backends wait between observations for
/// re-enumeration to settle.
pub fn usb_pnp_post_operation_poll_attempts() -> usize {
    super::selected::usb_pnp::usb_pnp_post_operation_poll_attempts()
}

/// How long the host backend waits between post-operation observations.
pub fn usb_pnp_post_operation_poll_interval() -> Duration {
    super::selected::usb_pnp::usb_pnp_post_operation_poll_interval()
}
