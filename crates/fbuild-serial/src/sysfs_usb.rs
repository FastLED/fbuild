//! Platform-neutral Linux `sysfs` USB topology/health parsing.
//!
//! `/sys/bus/usb/devices` exposes USB devices as a flat directory of
//! symlinks whose *names* encode bus topology (`"1-1"` is a device on root
//! port 1 of bus 1; `"1-1.4"` is a device on port 4 of the hub at
//! `"1-1"`). Each device directory nests its USB interfaces as real
//! subdirectories (Linux additionally names those `"<device>:<cfg>.<iface>"`
//! and colon-symlinks them at the top level too, but this module never
//! parses that name — see `scan_device_interfaces` for why; it is private, so
//! this is deliberately not an intra-doc link). This module
//! walks that shape and turns it into the same kind of facts Windows gets
//! from SetupAPI/CfgMgr32 (see the facade's Windows implementation,
//! `fbuild_core::platform::windows::device`) — but it never touches a live
//! filesystem itself. Every parsing function takes a `root: &Path`, so the
//! whole module is exercised with fixture trees built under a
//! `tempfile::TempDir` on any host OS, including this Windows dev box
//! (directory names containing `:` are not legal on Windows, which is
//! exactly why interface directories are identified structurally instead
//! of by name).
//!
//! The one host boundary is `live_root`, which asks
//! [`fbuild_core::platform::device::live_sysfs_usb_root`] whether this OS has
//! a live sysfs USB topology at all — everything else here is pure parsing
//! and runs (and is tested) everywhere.
//!
//! ## Design choices (FastLED/fbuild#1091)
//!
//! - **#1149 invariant preserved**: a device/tty is only ever classified as
//!   healthy or a problem when sysfs gives a *concrete* signal. Anything
//!   ambiguous (missing attribute files, a tty we can't map back to a
//!   device, a malformed directory) stays [`crate::ports::PortHealth::Unknown`]
//!   — never fabricated.
//! - **No fake Windows problem codes.** `PortHealth::PresentProblem`'s
//!   `problem_code` is a bare `u32` with no cross-platform meaning, so this
//!   module defines its own small, documented, Linux-only convention (the
//!   `LINUX_PROBLEM_*` constants below) rather than inventing a `CM_PROB_*`
//!   value that never came from Config Manager. `status` is always `None`
//!   on Linux — that field is a raw Windows `CM_Get_DevNode_Status` word.
//! - **`UsbProblemDevice` is not reused for Linux diagnostics.** Its shape
//!   (`device_class` = a Windows setup class, `instance_id` = a PnP
//!   instance path) doesn't have an honest Linux equivalent. Rather than
//!   stuff sysfs facts into Windows-shaped fields, Linux gets its own
//!   [`LinuxUsbProblemDevice`] summary type, consumed only for diagnostics
//!   (`fbuild port scan` / future CLI surfacing), not for the
//!   `PortHealth` selection path. `ports::present_usb_problem_devices()`
//!   stays an empty `Vec` on Linux for that reason; see
//!   `ports::present_usb_problem_devices_linux` for the sibling (Linux-only
//!   item, so not an intra-doc link — this module's docs build everywhere).
//!
//! ## macOS
//!
//! macOS exposes none of this via sysfs — the IOKit equivalent
//! (`IORegistryEntry` walking `IOUSBHostDevice`/`IOUSBHostInterface`) is a
//! distinct implementation that needs a Mac host to write and validate
//! against. Out of scope here; macOS stays `PortHealth::Unknown` /
//! `present_usb_problem_devices() == []`, which is the correct
//! never-fabricate behavior in the meantime (FastLED/fbuild#1091).

use fbuild_core::path::NormalizedPath;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::ports::PortHealth;

/// Default live sysfs USB topology root on Linux.
pub const DEFAULT_SYSFS_USB_ROOT: &str = "/sys/bus/usb/devices";

/// Device `authorized` attribute reads `0`: the kernel refused to
/// authorize/power the device (`usbcore.authorized_default=0`,
/// `deauthorize` via `usbguard`, etc). This is a concrete, unambiguous
/// fault signal.
pub const LINUX_PROBLEM_UNAUTHORIZED: u32 = 1;
/// Device `bConfigurationValue` reads `0`: the device is present but has no
/// active USB configuration (enumeration stalled before `SET_CONFIGURATION`
/// completed).
pub const LINUX_PROBLEM_UNCONFIGURED: u32 = 2;
/// A CDC Communications (`bInterfaceClass == 0x02`) or CDC Data
/// (`bInterfaceClass == 0x0a`) interface exists on the device but has no
/// `driver` symlink bound — the classic "device enumerated but
/// `cdc_acm`/`cdc_data` never attached" failure that leaves no `/dev/ttyACMx`.
pub const LINUX_PROBLEM_DRIVERLESS_CDC_INTERFACE: u32 = 3;

const CDC_COMMUNICATIONS_CLASS: u8 = 0x02;
const CDC_DATA_CLASS: u8 = 0x0a;

/// One USB interface directory (`"<device>:<config>.<interface>"`).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UsbInterfaceNode {
    /// sysfs directory name, e.g. `"1-1.4:1.0"`.
    pub dir_name: String,
    /// The owning device's directory name, e.g. `"1-1.4"`.
    pub device_dir_name: String,
    /// `bInterfaceClass`, parsed as hex (sysfs stores it as e.g. `"02"`).
    pub class: Option<u8>,
    /// Whether a `driver` symlink exists in the interface directory.
    pub driver_bound: bool,
    /// tty device names (e.g. `"ttyACM0"`) associated with this interface,
    /// from either a `tty/ttyACMx` subdirectory or (older kernels) a
    /// `ttyACMx` directory directly under the interface.
    pub tty_names: Vec<String>,
}

/// One USB device directory (`"<bus>-<port>[.<port>...]"` or `"usbN"` for a
/// root hub).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UsbDeviceNode {
    /// sysfs directory name, e.g. `"1-1.4"`.
    pub dir_name: String,
    pub busnum: Option<u32>,
    pub devnum: Option<u32>,
    pub vendor_id: Option<u16>,
    pub product_id: Option<u16>,
    pub serial: Option<String>,
    pub product: Option<String>,
    pub manufacturer: Option<String>,
    /// `authorized` attribute: `Some(true)` = `1`, `Some(false)` = `0`,
    /// `None` if the attribute file is absent (root hubs and some virtual
    /// devices never expose it).
    pub authorized: Option<bool>,
    /// `bConfigurationValue`; `Some(0)` means "enumerated but unconfigured".
    pub configuration_value: Option<u32>,
    /// Immediate parent device directory name, derived purely from the
    /// directory-name topology (no live syscalls). `None` for root hubs.
    pub parent_dir_name: Option<String>,
    /// `Some(true)` when the directory-name port path has 2+ segments,
    /// meaning at least one hub sits between this device and the root hub.
    /// `Some(false)` when it is directly on a root hub port. Mirrors the
    /// semantics of `classify_usb_ancestry` in the facade's Windows
    /// implementation:
    /// "is there a USB device ancestor before the root hub". Root hub
    /// entries (`"usbN"`) themselves get `None` — the question doesn't
    /// apply to the hub itself.
    pub behind_external_hub: Option<bool>,
    pub interfaces: Vec<UsbInterfaceNode>,
}

impl UsbDeviceNode {
    /// All tty names exposed by any interface on this device.
    pub fn tty_names(&self) -> impl Iterator<Item = &str> {
        self.interfaces
            .iter()
            .flat_map(|iface| iface.tty_names.iter().map(String::as_str))
    }

    /// Concrete, sysfs-observable fault signals for this device, each
    /// tagged with the `LINUX_PROBLEM_*` code and a short human reason.
    /// Empty means "no observed fault" — NOT "known healthy"; callers that
    /// need a positive healthy signal should also check
    /// [`Self::has_bound_tty_interface`].
    fn faults(&self) -> Vec<(u32, &'static str)> {
        let mut faults = Vec::new();
        if self.authorized == Some(false) {
            faults.push((LINUX_PROBLEM_UNAUTHORIZED, "device is unauthorized"));
        }
        if self.configuration_value == Some(0) {
            faults.push((
                LINUX_PROBLEM_UNCONFIGURED,
                "device has no active USB configuration",
            ));
        }
        if self.interfaces.iter().any(|iface| {
            !iface.driver_bound
                && matches!(
                    iface.class,
                    Some(CDC_COMMUNICATIONS_CLASS) | Some(CDC_DATA_CLASS)
                )
        }) {
            faults.push((
                LINUX_PROBLEM_DRIVERLESS_CDC_INTERFACE,
                "CDC interface has no bound driver",
            ));
        }
        faults
    }

    /// True when at least one interface has a bound driver and exposes a
    /// tty — the concrete "this device backs a usable serial port" signal.
    fn has_bound_tty_interface(&self) -> bool {
        self.interfaces
            .iter()
            .any(|iface| iface.driver_bound && !iface.tty_names.is_empty())
    }
}

/// Best-effort, diagnostics-only summary of a Linux USB device with an
/// observed fault. Deliberately **not** the Windows `UsbProblemDevice`
/// shape — see the module doc comment for why.
#[derive(Clone, Debug, PartialEq)]
pub struct LinuxUsbProblemDevice {
    /// sysfs directory name, e.g. `"1-1.4"`. The closest Linux analog to
    /// Windows' PnP instance id, but it is a topology path, not a stable
    /// device identity (it changes if the device is plugged into a
    /// different port).
    pub sysfs_path: String,
    pub problem_code: u32,
    pub reason: &'static str,
    pub vendor_id: Option<u16>,
    pub product_id: Option<u16>,
    pub product: Option<String>,
    pub behind_external_hub: Option<bool>,
    pub parent_sysfs_path: Option<String>,
}

fn read_attr(dir: &Path, name: &str) -> Option<String> {
    let value = fs::read_to_string(dir.join(name)).ok()?;
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn read_hex_u16(dir: &Path, name: &str) -> Option<u16> {
    u16::from_str_radix(&read_attr(dir, name)?, 16).ok()
}

fn read_hex_u8(dir: &Path, name: &str) -> Option<u8> {
    u8::from_str_radix(&read_attr(dir, name)?, 16).ok()
}

fn read_u32(dir: &Path, name: &str) -> Option<u32> {
    read_attr(dir, name)?.parse().ok()
}

fn read_bool_flag(dir: &Path, name: &str) -> Option<bool> {
    match read_attr(dir, name)?.as_str() {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }
}

/// Split a device directory name into `(depth, parent_dir_name)`.
///
/// - `"usb1"` (a root hub): depth `0`, no parent.
/// - `"1-1"` (directly on a root hub port): depth `1`, parent `"usb1"`.
/// - `"1-1.4"`: depth `2`, parent `"1-1"`.
/// - `"1-1.4.2"`: depth `3`, parent `"1-1.4"`.
///
/// Returns `None` for names that don't match the `"<bus>-<port>[.<port>..]"`
/// or `"usb<bus>"` shape (malformed/unexpected entries are skipped by the
/// caller, never guessed at).
fn topology(dir_name: &str) -> Option<(usize, Option<String>)> {
    if let Some(bus) = dir_name.strip_prefix("usb") {
        if !bus.is_empty() && bus.chars().all(|c| c.is_ascii_digit()) {
            return Some((0, None));
        }
        return None;
    }
    let (bus, port_path) = dir_name.split_once('-')?;
    if bus.is_empty() || !bus.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if port_path.is_empty() {
        return None;
    }
    let segments: Vec<&str> = port_path.split('.').collect();
    if segments
        .iter()
        .any(|seg| seg.is_empty() || !seg.chars().all(|c| c.is_ascii_digit()))
    {
        // Not a plain numeric port path — e.g. a flat interface symlink
        // name like "1-1:1.0" splits into a `port_path` of "1:1.0", whose
        // first segment ("1:1") isn't numeric. Reject rather than
        // misparse it as a device.
        return None;
    }
    let depth = segments.len();
    let parent = if depth == 1 {
        format!("usb{bus}")
    } else {
        format!("{bus}-{}", segments[..depth - 1].join("."))
    };
    Some((depth, Some(parent)))
}

/// On real Linux, USB interface directories are named
/// `"<device>:<cfg>.<iface>"` (a literal colon) and appear both as flat
/// top-level symlinks *and* as real subdirectories nested inside their
/// owning device's directory. Colon is not a legal path character on
/// Windows, so this module deliberately does **not** parse interfaces by
/// name at all — it only ever looks at nested subdirectories of a device
/// directory and identifies an interface *structurally*, by the presence
/// of a `bInterfaceClass` attribute file. This is accurate for a real
/// Linux sysfs tree (interfaces are always nested under their device
/// there) and lets fixture trees on any OS — including Windows — use
/// whatever directory names the local filesystem allows.
fn scan_device_interfaces(device_dir_name: &str, device_dir: &Path) -> Vec<UsbInterfaceNode> {
    let Ok(entries) = fs::read_dir(device_dir) else {
        return Vec::new();
    };
    let mut interfaces = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if !path.join("bInterfaceClass").is_file() {
            // Not an interface directory (could be "tty", "power",
            // "driver", or an unrelated subdirectory) — skip.
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        interfaces.push(UsbInterfaceNode {
            dir_name: name,
            device_dir_name: device_dir_name.to_string(),
            class: read_hex_u8(&path, "bInterfaceClass"),
            driver_bound: path.join("driver").exists(),
            tty_names: interface_tty_names(&path),
        });
    }
    interfaces
}

/// tty names directly exposed by an interface directory: either a `tty`
/// subdirectory whose entries are the tty names (modern kernels), or (older
/// kernels / some drivers) `ttyACMx`/`ttyUSBx`-named directories directly
/// under the interface.
fn interface_tty_names(iface_dir: &Path) -> Vec<String> {
    let mut names = Vec::new();
    let tty_subdir = iface_dir.join("tty");
    if let Ok(entries) = fs::read_dir(&tty_subdir) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                names.push(name.to_string());
            }
        }
    }
    if let Ok(entries) = fs::read_dir(iface_dir) {
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if (name.starts_with("ttyACM") || name.starts_with("ttyUSB")) && !names.contains(&name)
            {
                names.push(name);
            }
        }
    }
    names
}

/// Walk `root` (a `/sys/bus/usb/devices`-shaped directory) and return every
/// device it can parse, with interfaces attached and topology resolved.
///
/// Never panics: unreadable directories, malformed names, and missing
/// attribute files are all treated as "no data" and skipped or left `None`
/// — this is pure, fixture-testable parsing with no live-filesystem
/// assumptions beyond "root exists and is a directory" (if it doesn't,
/// this simply returns an empty `Vec`).
pub fn scan_usb_devices(root: &Path) -> Vec<UsbDeviceNode> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };

    let mut device_dirs: Vec<(String, NormalizedPath)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        // Flat top-level interface symlinks (real Linux name: e.g.
        // `"1-1:1.0"`) are never treated as devices — `topology()` already
        // rejects their non-numeric port-path segment, but skip them
        // up-front too so a colon-containing name never becomes a
        // `HashMap` key. Interfaces are only ever discovered by nesting
        // (see `scan_device_interfaces`), never by this top-level name.
        if name.contains(':') {
            continue;
        }
        device_dirs.push((name, path.into()));
    }

    let mut devices: HashMap<String, UsbDeviceNode> = HashMap::new();
    for (dir_name, dir) in &device_dirs {
        let Some((depth, parent_dir_name)) = topology(dir_name) else {
            continue;
        };
        let behind_external_hub = if depth == 0 { None } else { Some(depth >= 2) };
        devices.insert(
            dir_name.clone(),
            UsbDeviceNode {
                dir_name: dir_name.clone(),
                busnum: read_u32(dir, "busnum"),
                devnum: read_u32(dir, "devnum"),
                vendor_id: read_hex_u16(dir, "idVendor"),
                product_id: read_hex_u16(dir, "idProduct"),
                serial: read_attr(dir, "serial"),
                product: read_attr(dir, "product"),
                manufacturer: read_attr(dir, "manufacturer"),
                authorized: read_bool_flag(dir, "authorized"),
                configuration_value: read_u32(dir, "bConfigurationValue"),
                parent_dir_name,
                behind_external_hub,
                interfaces: scan_device_interfaces(dir_name, dir),
            },
        );
    }

    let mut result: Vec<UsbDeviceNode> = devices.into_values().collect();
    result.sort_by(|a, b| a.dir_name.cmp(&b.dir_name));
    result
}

/// Diagnostics-only: every device under `root` with at least one concrete
/// sysfs fault signal (see the `LINUX_PROBLEM_*` constants). A device with
/// multiple faults appears once per fault.
pub fn linux_usb_problem_devices_from_root(root: &Path) -> Vec<LinuxUsbProblemDevice> {
    let devices = scan_usb_devices(root);
    let mut problems = Vec::new();
    for device in &devices {
        for (problem_code, reason) in device.faults() {
            problems.push(LinuxUsbProblemDevice {
                sysfs_path: device.dir_name.clone(),
                problem_code,
                reason,
                vendor_id: device.vendor_id,
                product_id: device.product_id,
                product: device.product.clone(),
                behind_external_hub: device.behind_external_hub,
                parent_sysfs_path: device.parent_dir_name.clone(),
            });
        }
    }
    problems
}

/// Classify the health of a single tty device name (e.g. `"ttyACM0"`,
/// *not* `"/dev/ttyACM0"`) against the devices found under `root`.
///
/// Per the #1149 invariant: only returns `HealthyPresent` /
/// `PresentProblem` when a device under `root` concretely owns that tty.
/// If no device claims the tty (not found, or found but neither a bound
/// tty interface nor a fault — e.g. a non-USB tty), returns `Unknown`
/// rather than guessing.
pub fn health_for_tty_from_root(root: &Path, tty_name: &str) -> PortHealth {
    let devices = scan_usb_devices(root);
    let Some(device) = devices
        .iter()
        .find(|device| device.tty_names().any(|name| name == tty_name))
    else {
        return PortHealth::Unknown;
    };

    let faults = device.faults();
    if let Some(&(problem_code, _reason)) = faults.first() {
        return PortHealth::PresentProblem {
            problem_code,
            status: None,
        };
    }
    if device.has_bound_tty_interface() {
        return PortHealth::HealthyPresent;
    }
    PortHealth::Unknown
}

/// Live sysfs root when the host provides one (`Some` only on Linux, where
/// `/sys/bus/usb/devices` exists). Every other function in this module takes
/// an explicit `root` and has no idea what OS it's running on.
pub fn live_root() -> Option<NormalizedPath> {
    fbuild_core::platform::device::live_sysfs_usb_root()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Minimal fixture builder for a `/sys/bus/usb/devices`-shaped tree.
    struct SysfsFixture {
        root: TempDir,
    }

    impl SysfsFixture {
        fn new() -> Self {
            Self {
                root: TempDir::new().expect("tempdir"),
            }
        }

        fn root_path(&self) -> &Path {
            self.root.path()
        }

        fn device_dir(&self, name: &str) -> NormalizedPath {
            let dir = self.root.path().join(name);
            fs::create_dir_all(&dir).expect("mkdir device");
            NormalizedPath::from(dir)
        }

        fn write_attr(&self, dir: &Path, name: &str, value: &str) {
            fs::write(dir.join(name), value).expect("write attr");
        }

        /// A root hub, e.g. `usb1`.
        fn root_hub(&self, name: &str, vendor: &str, product: &str) -> NormalizedPath {
            let dir = self.device_dir(name);
            self.write_attr(&dir, "idVendor", vendor);
            self.write_attr(&dir, "idProduct", product);
            self.write_attr(&dir, "busnum", "1");
            self.write_attr(&dir, "devnum", "1");
            self.write_attr(&dir, "bConfigurationValue", "1");
            self.write_attr(&dir, "authorized", "1");
            dir
        }

        /// A plain device directory with common attributes; caller adds
        /// interfaces separately.
        fn device(
            &self,
            name: &str,
            vid: &str,
            pid: &str,
            serial: Option<&str>,
            product: Option<&str>,
        ) -> NormalizedPath {
            let dir = self.device_dir(name);
            self.write_attr(&dir, "idVendor", vid);
            self.write_attr(&dir, "idProduct", pid);
            self.write_attr(&dir, "busnum", "1");
            self.write_attr(&dir, "devnum", "5");
            self.write_attr(&dir, "bConfigurationValue", "1");
            self.write_attr(&dir, "authorized", "1");
            if let Some(serial) = serial {
                self.write_attr(&dir, "serial", serial);
            }
            if let Some(product) = product {
                self.write_attr(&dir, "product", product);
            }
            dir
        }

        /// Add a nested interface subdirectory under `device_name` with a
        /// bound driver and a tty (the healthy CDC ACM case). Interface
        /// directories are identified structurally (presence of
        /// `bInterfaceClass`), not by name, so the fixture name just needs
        /// to be a legal directory name on every OS — real Linux would
        /// call this `"<device>:1.<n>"`, but colons aren't legal on
        /// Windows and the parser never looks at this name.
        fn cdc_control_interface_with_tty(&self, device_name: &str, iface_num: u32, tty: &str) {
            let iface = self
                .root
                .path()
                .join(device_name)
                .join(format!("iface-1.{iface_num}"));
            fs::create_dir_all(&iface).expect("mkdir iface");
            self.write_attr(&iface, "bInterfaceClass", "02");
            fs::create_dir_all(iface.join("driver")).expect("mkdir driver");
            fs::create_dir_all(iface.join("tty").join(tty)).expect("mkdir tty");
        }

        /// CDC interface with no bound driver.
        fn cdc_control_interface_driverless(&self, device_name: &str, iface_num: u32) {
            let iface = self
                .root
                .path()
                .join(device_name)
                .join(format!("iface-1.{iface_num}"));
            fs::create_dir_all(&iface).expect("mkdir iface");
            self.write_attr(&iface, "bInterfaceClass", "02");
        }
    }

    // ---- healthy CDC device -------------------------------------------

    #[test]
    fn healthy_cdc_device_with_bound_driver_and_tty_is_healthy_present() {
        let fx = SysfsFixture::new();
        fx.root_hub("usb1", "1d6b", "0002");
        fx.device(
            "1-1",
            "2e8a",
            "000a",
            Some("E1234567890"),
            Some("Board CDC"),
        );
        fx.cdc_control_interface_with_tty("1-1", 0, "ttyACM0");

        let health = health_for_tty_from_root(fx.root_path(), "ttyACM0");
        assert_eq!(health, PortHealth::HealthyPresent);
    }

    #[test]
    fn healthy_device_reports_not_behind_external_hub_when_on_root_port() {
        let fx = SysfsFixture::new();
        fx.root_hub("usb1", "1d6b", "0002");
        fx.device("1-1", "2e8a", "000a", Some("SN1"), Some("Board"));
        fx.cdc_control_interface_with_tty("1-1", 0, "ttyACM0");

        let devices = scan_usb_devices(fx.root_path());
        let device = devices
            .iter()
            .find(|d| d.dir_name == "1-1")
            .expect("device");
        assert_eq!(device.behind_external_hub, Some(false));
        assert_eq!(device.parent_dir_name.as_deref(), Some("usb1"));
    }

    // ---- topology: behind external hub vs root port -------------------

    #[test]
    fn device_behind_external_hub_is_classified_correctly() {
        let fx = SysfsFixture::new();
        fx.root_hub("usb1", "1d6b", "0002");
        // "1-1" is an external hub itself (no CDC interface, just present).
        fx.device("1-1", "0424", "2514", None, Some("USB Hub"));
        // "1-1.4" hangs off port 4 of that hub.
        fx.device("1-1.4", "2e8a", "000a", Some("SN2"), Some("Board"));
        fx.cdc_control_interface_with_tty("1-1.4", 0, "ttyACM1");

        let devices = scan_usb_devices(fx.root_path());
        let hub = devices.iter().find(|d| d.dir_name == "1-1").expect("hub");
        assert_eq!(hub.behind_external_hub, Some(false));

        let leaf = devices
            .iter()
            .find(|d| d.dir_name == "1-1.4")
            .expect("leaf");
        assert_eq!(leaf.behind_external_hub, Some(true));
        assert_eq!(leaf.parent_dir_name.as_deref(), Some("1-1"));

        let health = health_for_tty_from_root(fx.root_path(), "ttyACM1");
        assert_eq!(health, PortHealth::HealthyPresent);
    }

    #[test]
    fn root_hub_itself_has_no_behind_external_hub_classification() {
        let fx = SysfsFixture::new();
        fx.root_hub("usb1", "1d6b", "0002");
        let devices = scan_usb_devices(fx.root_path());
        let hub = devices.iter().find(|d| d.dir_name == "usb1").expect("hub");
        assert_eq!(hub.behind_external_hub, None);
        assert_eq!(hub.parent_dir_name, None);
    }

    // ---- unauthorized device -------------------------------------------

    #[test]
    fn unauthorized_device_is_a_present_problem() {
        let fx = SysfsFixture::new();
        fx.root_hub("usb1", "1d6b", "0002");
        let dir = fx.device("1-1", "2e8a", "000a", Some("SN3"), Some("Board"));
        fx.write_attr(&dir, "authorized", "0");
        fx.cdc_control_interface_with_tty("1-1", 0, "ttyACM2");

        let health = health_for_tty_from_root(fx.root_path(), "ttyACM2");
        assert_eq!(
            health,
            PortHealth::PresentProblem {
                problem_code: LINUX_PROBLEM_UNAUTHORIZED,
                status: None,
            }
        );

        let problems = linux_usb_problem_devices_from_root(fx.root_path());
        assert!(
            problems
                .iter()
                .any(|p| p.sysfs_path == "1-1" && p.problem_code == LINUX_PROBLEM_UNAUTHORIZED)
        );
    }

    // ---- unconfigured device --------------------------------------------

    #[test]
    fn unconfigured_device_is_a_present_problem() {
        let fx = SysfsFixture::new();
        fx.root_hub("usb1", "1d6b", "0002");
        let dir = fx.device("1-1", "2e8a", "000a", Some("SN4"), Some("Board"));
        fx.write_attr(&dir, "bConfigurationValue", "0");

        let problems = linux_usb_problem_devices_from_root(fx.root_path());
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].problem_code, LINUX_PROBLEM_UNCONFIGURED);
        assert_eq!(problems[0].sysfs_path, "1-1");
    }

    // ---- interface lacking driver binding -------------------------------

    #[test]
    fn cdc_interface_without_driver_is_a_present_problem_and_not_healthy() {
        let fx = SysfsFixture::new();
        fx.root_hub("usb1", "1d6b", "0002");
        fx.device("1-1", "2e8a", "000a", Some("SN5"), Some("Board"));
        fx.cdc_control_interface_driverless("1-1", 0);

        let problems = linux_usb_problem_devices_from_root(fx.root_path());
        assert_eq!(problems.len(), 1);
        assert_eq!(
            problems[0].problem_code,
            LINUX_PROBLEM_DRIVERLESS_CDC_INTERFACE
        );

        // No tty exists (interface has no driver, no tty subdir) so there's
        // nothing to classify by tty name — that's expected; the interface
        // fault is diagnostic-only until/unless a tty shows up.
        let health = health_for_tty_from_root(fx.root_path(), "ttyACM0");
        assert_eq!(health, PortHealth::Unknown);
    }

    // ---- malformed / partial entries never panic -------------------------

    #[test]
    fn device_missing_id_vendor_is_skipped_but_does_not_panic() {
        let fx = SysfsFixture::new();
        fx.root_hub("usb1", "1d6b", "0002");
        // No idVendor/idProduct written at all.
        let dir = fx.device_dir("1-1");
        fx.write_attr(&dir, "busnum", "1");

        let devices = scan_usb_devices(fx.root_path());
        let device = devices
            .iter()
            .find(|d| d.dir_name == "1-1")
            .expect("still parsed");
        assert_eq!(device.vendor_id, None);
        assert_eq!(device.product_id, None);
        // No faults should be derived from missing (not zero) attributes.
        assert!(device.faults().is_empty());
    }

    #[test]
    fn malformed_device_directory_name_is_skipped_without_panicking() {
        let fx = SysfsFixture::new();
        fx.root_hub("usb1", "1d6b", "0002");
        // Not a valid "<bus>-<port>" or "usbN" shape.
        fx.device_dir("not-a-real-device-name!!");
        fx.device_dir("");

        let devices = scan_usb_devices(fx.root_path());
        assert!(devices.iter().any(|d| d.dir_name == "usb1"));
        assert!(
            !devices
                .iter()
                .any(|d| d.dir_name == "not-a-real-device-name!!")
        );
    }

    #[test]
    fn interface_nested_under_malformed_device_directory_is_dropped() {
        let fx = SysfsFixture::new();
        fx.root_hub("usb1", "1d6b", "0002");
        // A device-shaped directory whose name doesn't parse as valid
        // topology (so it's skipped entirely), with a would-be interface
        // nested inside it.
        let bad_device = fx.root_path().join("not-a-real-device-name!!");
        let iface = bad_device.join("iface-1.0");
        fs::create_dir_all(&iface).unwrap();
        fs::write(iface.join("bInterfaceClass"), "02").unwrap();

        // Must not panic, and must not surface a device for the malformed
        // name or attach its interface anywhere.
        let devices = scan_usb_devices(fx.root_path());
        assert!(
            !devices
                .iter()
                .any(|d| d.dir_name == "not-a-real-device-name!!")
        );
        assert!(devices.iter().all(|d| d.interfaces.is_empty()));
    }

    #[test]
    fn empty_or_missing_root_returns_empty_without_panicking() {
        let fx = SysfsFixture::new();
        assert!(scan_usb_devices(fx.root_path()).is_empty());
        assert!(scan_usb_devices(&fx.root_path().join("does-not-exist")).is_empty());
        assert_eq!(
            health_for_tty_from_root(&fx.root_path().join("nope"), "ttyACM0"),
            PortHealth::Unknown
        );
    }

    // ---- RP2040-typical VID/PID (test-only synthetic fixture) -----------

    #[test]
    fn rp2040_typical_vid_pid_device_parses_cleanly() {
        // 2e8a = Raspberry Pi Trading Ltd; test-only fixture identity, never
        // a runtime default (fbuild's real VID/PID data comes from the
        // FastLED/boards registry per CLAUDE.md's USB VID/PID rule).
        let fx = SysfsFixture::new();
        fx.root_hub("usb1", "1d6b", "0002");
        fx.device(
            "1-1",
            "2e8a",
            "000a",
            Some("E6612483D3591234"),
            Some("Board CDC"),
        );
        fx.cdc_control_interface_with_tty("1-1", 0, "ttyACM0");

        let devices = scan_usb_devices(fx.root_path());
        let device = devices
            .iter()
            .find(|d| d.dir_name == "1-1")
            .expect("device");
        assert_eq!(device.vendor_id, Some(0x2e8a));
        assert_eq!(device.product_id, Some(0x000a));
        assert_eq!(device.serial.as_deref(), Some("E6612483D3591234"));
    }

    // ---- tty unrelated to any USB device stays Unknown -------------------

    #[test]
    fn tty_not_owned_by_any_device_is_unknown() {
        let fx = SysfsFixture::new();
        fx.root_hub("usb1", "1d6b", "0002");
        fx.device("1-1", "2e8a", "000a", Some("SN6"), Some("Board"));
        fx.cdc_control_interface_with_tty("1-1", 0, "ttyACM0");

        // ttyS0 (a plain platform UART) is never claimed by any USB device.
        let health = health_for_tty_from_root(fx.root_path(), "ttyS0");
        assert_eq!(health, PortHealth::Unknown);
    }
}
