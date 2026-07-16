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

/// Enumerate every serial port currently visible to the OS.
///
/// Unlike [`serialport::available_ports`], on Windows this includes ports
/// whose devnode status is not "OK" (the Teensy / composite-device case).
pub fn available_ports() -> serialport::Result<Vec<serialport::SerialPortInfo>> {
    #[cfg(windows)]
    {
        imp::available_ports()
    }
    #[cfg(not(windows))]
    {
        serialport::available_ports()
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
}

/// Best-effort enumeration of present USB devnodes with a non-zero Windows
/// problem code.  This is empty on non-Windows hosts and never makes a port
/// scan fail merely because host diagnostics are unavailable.
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

#[cfg(windows)]
mod imp {
    use super::UsbProblemDevice;
    use std::collections::HashSet;
    use std::ptr;

    use serialport::{SerialPortInfo, SerialPortType, UsbPortInfo};
    use windows_sys::core::GUID;
    use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
        CM_Get_DevNode_Status, CM_Get_Device_IDW, CM_Get_Parent, SetupDiClassGuidsFromNameW,
        SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW,
        SetupDiGetDeviceInstanceIdW, SetupDiGetDeviceRegistryPropertyW, SetupDiOpenDevRegKey,
        CR_SUCCESS, DICS_FLAG_GLOBAL, DIGCF_PRESENT, DIREG_DEV, GUID_DEVCLASS_USB, HDEVINFO,
        MAX_DEVICE_ID_LEN, SPDRP_FRIENDLYNAME, SPDRP_HARDWAREID,
        SPDRP_LOCATION_INFORMATION, SPDRP_MFG, SP_DEVINFO_DATA,
    };
    use windows_sys::Win32::Foundation::{FALSE, FILETIME, INVALID_HANDLE_VALUE, MAX_PATH};
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegEnumValueW, RegOpenKeyExW, RegQueryInfoKeyW, RegQueryValueExW, HKEY,
        HKEY_LOCAL_MACHINE, KEY_READ, REG_SZ,
    };

    const CONNECTOR_PUNCTUATION_SELECTION: &[char] = &[':', '_', '\u{ff3f}'];

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

        /// True when the devnode is instantiated in the live device tree.
        /// A phantom devnode (unplugged, or a Status=Unknown Teensy interface
        /// whose function driver never started) returns `CR_NO_SUCH_DEVINST`
        /// here, i.e. `false`.
        fn present(&mut self) -> bool {
            let mut status = 0u32;
            let mut problem = 0u32;
            let res = unsafe {
                CM_Get_DevNode_Status(&mut status, &mut problem, self.devinfo_data.DevInst, 0)
            };
            res == CR_SUCCESS
        }

        fn port_type(&mut self) -> SerialPortType {
            self.instance_id()
                .map(|s| (s, self.parent_instance_id()))
                .and_then(|(d, p)| parse_usb_port_info(&d, p.as_deref()))
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
            let result = unsafe {
                CM_Get_Device_IDW(parent, buffer.as_mut_ptr(), buffer.len() as u32, 0)
            };
            if result != CR_SUCCESS {
                break;
            }
            let length = buffer.iter().position(|&unit| unit == 0).unwrap_or(buffer.len());
            ids.push(String::from_utf16_lossy(&buffer[..length]));
            current = parent;
        }
        ids
    }

    fn classify_usb_ancestry(devinst: u32) -> Option<bool> {
        let ancestors = ancestor_ids(devinst);
        let root_index = ancestors.iter().position(|id| {
            id.to_ascii_uppercase().starts_with("USB\\ROOT_HUB")
        })?;
        Some(ancestors[..root_index].iter().any(|id| {
            let upper = id.to_ascii_uppercase();
            upper.starts_with("USB\\VID_") && upper.contains("&PID_")
        }))
    }

    pub(super) fn present_usb_problem_devices() -> Vec<UsbProblemDevice> {
        let hdi = unsafe {
            SetupDiGetClassDevsW(
                &GUID_DEVCLASS_USB,
                std::ptr::null(),
                0,
                DIGCF_PRESENT,
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
            });
        }
        unsafe {
            SetupDiDestroyDeviceInfoList(hdi);
        }
        devices
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
        let length = buffer.iter().position(|&unit| unit == 0).unwrap_or(buffer.len());
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
        let length = buffer.iter().position(|&unit| unit == 0).unwrap_or(buffer.len());
        let value = String::from_utf16_lossy(&buffer[..length]);
        (!value.is_empty()).then_some(value)
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

    pub(super) fn available_ports() -> serialport::Result<Vec<SerialPortInfo>> {
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
                let present = port_device.present();
                let port_type = port_device.port_type();
                let is_usb = matches!(port_type, SerialPortType::UsbPort(_));
                // Include every present port (unchanged behaviour), PLUS
                // non-present USB serial ports — the Status=Unknown Teensy
                // case the whole fix exists for. A non-present *non-USB*
                // devnode is a stale phantom with no VID:PID to act on, so we
                // leave it out to avoid resurrecting ancient ACPI/BT junk.
                // FastLED/fbuild#962.
                if !present && !is_usb {
                    continue;
                }
                // A phantom devnode can be enumerated once per matching class
                // GUID; de-dup on the COM name.
                if !seen.insert(port_name.clone()) {
                    continue;
                }
                ports.push(SerialPortInfo {
                    port_name,
                    port_type,
                });
            }
        }

        // Fold in any DEVICEMAP\SERIALCOMM ports not already found.
        for raw_port in get_registry_com_ports() {
            if seen.insert(raw_port.clone()) {
                ports.push(SerialPortInfo {
                    port_name: raw_port,
                    port_type: SerialPortType::Unknown,
                });
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
