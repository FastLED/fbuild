//! Selected Windows serial-port enumeration mechanics behind
//! [`crate::platform::device`] — a SetupAPI/CfgMgr32 fork of
//! `serialport` 4.9's `windows/enumerate.rs` (MIT/Apache-2.0) with one
//! behavioural change: devnodes whose `CM_Get_DevNode_Status` reports a
//! **non-OK problem code** are still listed. `serialport` skips any such
//! devnode, and PJRC/Teensy (VID `16C0`) serial functions enumerate as
//! composite `MI_00` interfaces that commonly report `Status = Unknown`,
//! so upstream drops **every** Teensy COM port — a physically-attached
//! Teensy is invisible to port discovery. FastLED/fbuild#962.
//!
//! USB PnP diagnostics/recovery live in [`super::usb_pnp`]; the two
//! modules share the SetupAPI helpers re-exported here.

use std::collections::HashSet;
use std::io;
use std::ptr;

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_DevNode_Status, CM_Get_Device_IDW, CM_Get_Parent, CR_NO_SUCH_DEVINST, CR_SUCCESS,
    DICS_FLAG_GLOBAL, DIREG_DEV, HDEVINFO, MAX_DEVICE_ID_LEN, SP_DEVINFO_DATA,
    SPDRP_FRIENDLYNAME, SPDRP_HARDWAREID, SPDRP_MFG, SetupDiClassGuidsFromNameW,
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW,
    SetupDiGetDeviceInstanceIdW, SetupDiGetDevicePropertyW, SetupDiGetDeviceRegistryPropertyW,
    SetupDiOpenDevRegKey,
};
use windows_sys::Win32::Devices::Properties::{
    DEVPKEY_Device_LocationPaths, DEVPROP_TYPE_STRING_LIST,
};
use windows_sys::Win32::Foundation::{FALSE, FILETIME, INVALID_HANDLE_VALUE, MAX_PATH};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_READ, REG_SZ, RegCloseKey, RegEnumValueW, RegOpenKeyExW,
    RegQueryInfoKeyW, RegQueryValueExW,
};
use windows_sys::core::GUID;

use crate::path::NormalizedPath;
use crate::platform::device::{
    DevNodeObservation, KernelDriverClass, SerialPortFacts, SerialPortTypeFacts,
    UsbSerialIdentityFacts,
};

const CONNECTOR_PUNCTUATION_SELECTION: &[char] = &[':', '_', '\u{ff3f}'];

pub(crate) fn available_serial_ports() -> io::Result<Vec<SerialPortFacts>> {
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
            let observation = port_device.pnp_observation();
            let port_type =
                port_device.port_type(instance_id.as_deref(), parent_instance_id.as_deref());
            let is_usb = matches!(port_type, SerialPortTypeFacts::Usb(_));
            // Include every present port (unchanged behaviour), PLUS
            // non-present USB serial ports — the Status=Unknown Teensy
            // case the whole fix exists for. A non-present *non-USB*
            // devnode is a stale phantom with no VID:PID to act on, so we
            // leave it out to avoid resurrecting ancient ACPI/BT junk.
            // FastLED/fbuild#962.
            if matches!(observation, DevNodeObservation::Phantom) && !is_usb {
                continue;
            }
            // A phantom devnode can be enumerated once per matching class
            // GUID; de-dup on the COM name.
            if !seen.insert(port_name.clone()) {
                continue;
            }
            ports.push(SerialPortFacts {
                port_name,
                port_type,
                observation,
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
            ports.push(SerialPortFacts {
                port_name: raw_port,
                port_type: SerialPortTypeFacts::Unknown,
                observation: DevNodeObservation::Unknown,
                instance_id: None,
                parent_instance_id: None,
                ancestor_instance_ids: Vec::new(),
                location_paths: Vec::new(),
            });
        }
    }
    Ok(ports)
}

pub(crate) fn detect_serial_kernel_driver(_port_name: &str) -> Option<KernelDriverClass> {
    // Windows SetupDi detection (SPDRP_SERVICE) is a documented #895
    // follow-up. Returning None here preserves the caller fallback chain
    // so Windows behaviour cannot regress.
    None
}

pub(crate) fn live_sysfs_usb_root() -> Option<NormalizedPath> {
    None
}

pub(super) fn as_utf16(utf8: &str) -> Vec<u16> {
    utf8.encode_utf16().chain(Some(0)).collect()
}

fn from_utf16_lossy_trimmed(utf16: &[u16]) -> String {
    String::from_utf16_lossy(utf16)
        .trim_end_matches(0 as char)
        .to_string()
}

fn get_ports_guids() -> io::Result<Vec<GUID>> {
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
                return Err(io::Error::other("Unable to determine number of Ports GUIDs"));
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

/// Parse a Windows HWID string into [`UsbSerialIdentityFacts`] (with the
/// composite `MI_xx` interface index preserved). Pure — unit-tested below.
///
/// VID/PID always come from the device's own hardware id (a composite
/// interface's `MI_xx` hwid carries the same VID/PID as its parent). Only
/// the serial number is taken from the parent for composite devices — and
/// if the parent isn't available (a **phantom** devnode whose live parent
/// no longer exists, i.e. the Status=Unknown Teensy case) we fall back to
/// the child's own serial tail rather than giving up. This is the key
/// difference from upstream serialport, which returns `None` (→ no VID/PID)
/// for a composite devnode with no reachable parent. FastLED/fbuild#962.
pub(super) fn parse_usb_port_info(
    hardware_id: &str,
    parent_hardware_id: Option<&str>,
) -> Option<UsbSerialIdentityFacts> {
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

    Some(UsbSerialIdentityFacts {
        vid: u16::from_str_radix(child.vid, 16).ok()?,
        pid: u16::from_str_radix(child.pid, 16).ok()?,
        serial_number: serial.map(str::to_string),
        manufacturer: None,
        product: None,
        // The workspace enables serialport's `usbportinfo-interface`
        // feature precisely so this field exists downstream; it carries
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
        if value_type != REG_SZ || !byte_len.is_multiple_of(2) || byte_len > buffer_byte_len {
            return String::new();
        }
        let len = buffer_byte_len as usize / 2;
        let port_name = &port_name_buffer[0..len];
        from_utf16_lossy_trimmed(port_name)
    }

    /// Read the Config Manager observation without flattening its three
    /// important outcomes.  A missing live devnode is a phantom; a query
    /// failure that is not that explicit state remains unknown.
    fn pnp_observation(&mut self) -> DevNodeObservation {
        let mut status = 0u32;
        let mut problem = 0u32;
        // SAFETY: `DevInst` comes from the live SetupAPI record and both
        // output pointers reference initialized writable local storage.
        let res = unsafe {
            CM_Get_DevNode_Status(&mut status, &mut problem, self.devinfo_data.DevInst, 0)
        };
        if res == CR_SUCCESS {
            DevNodeObservation::Present {
                status,
                problem_code: problem,
            }
        } else if res == CR_NO_SUCH_DEVINST {
            DevNodeObservation::Phantom
        } else {
            DevNodeObservation::Unknown
        }
    }

    fn port_type(
        &mut self,
        instance_id: Option<&str>,
        parent_instance_id: Option<&str>,
    ) -> SerialPortTypeFacts {
        instance_id
            .and_then(|id| parse_usb_port_info(id, parent_instance_id))
            .map(|mut facts: UsbSerialIdentityFacts| {
                facts.manufacturer = self.property(SPDRP_MFG);
                facts.product = self.property(SPDRP_FRIENDLYNAME);
                SerialPortTypeFacts::Usb(facts)
            })
            .unwrap_or(SerialPortTypeFacts::Unknown)
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

pub(super) fn ancestor_ids(devinst: u32) -> Vec<String> {
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

pub(super) fn location_paths_from_info(hdi: HDEVINFO, info: &SP_DEVINFO_DATA) -> Vec<String> {
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
                || !byte_len.is_multiple_of(2)
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

    #[test]
    fn kernel_driver_detection_is_deferred_on_windows() {
        // Documented #895 follow-up (SetupDi SPDRP_SERVICE). Until it
        // lands, the path returns None so the existing fallback chain
        // stays in charge — no regression risk.
        assert_eq!(detect_serial_kernel_driver("COM3"), None);
        assert_eq!(detect_serial_kernel_driver("COM42"), None);
    }
}
