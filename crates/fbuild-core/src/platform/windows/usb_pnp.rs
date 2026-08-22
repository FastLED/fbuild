//! Selected Windows USB PnP diagnostics and recovery mechanics behind
//! [`crate::platform::device`]: problem-device enumeration, the Pico SDK
//! BOOTSEL reset interface (WinUsb control transfer), and the CfgMgr32
//! inspect/re-enumerate/restart primitives the recovery ladder drives.
//!
//! Split from serial-port enumeration (`super::device`) purely for size;
//! both halves share the SetupAPI/CfgMgr32 helpers re-exported there.

use std::collections::HashMap;
use std::io;
use std::time::Duration;

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Disable_DevNode, CM_Enable_DevNode, CM_Get_DevNode_PropertyW, CM_Get_DevNode_Status,
    CM_Get_Device_IDW, CM_Get_Parent, CM_LOCATE_DEVNODE_NORMAL, CM_LOCATE_DEVNODE_PHANTOM,
    CM_Locate_DevNodeW, CM_Reenumerate_DevNode, CR_NO_SUCH_DEVINST, CR_NO_SUCH_VALUE, CR_SUCCESS,
    DIGCF_ALLCLASSES, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, HDEVINFO, MAX_DEVICE_ID_LEN,
    SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W, SP_DEVINFO_DATA, SPDRP_CLASS,
    SPDRP_COMPATIBLEIDS, SPDRP_FRIENDLYNAME, SPDRP_LOCATION_INFORMATION,
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiEnumDeviceInterfaces,
    SetupDiGetClassDevsW, SetupDiGetDeviceInstanceIdW, SetupDiGetDeviceInterfaceDetailW,
    SetupDiGetDeviceRegistryPropertyW,
};
use windows_sys::Win32::Devices::Properties::{
    DEVPKEY_Device_Class, DEVPKEY_Device_LocationPaths, DEVPROP_TYPE_STRING,
    DEVPROP_TYPE_STRING_LIST,
};
use windows_sys::Win32::Devices::Usb::{
    WINUSB_INTERFACE_HANDLE, WINUSB_SETUP_PACKET, WinUsb_ControlTransfer, WinUsb_Free,
    WinUsb_Initialize,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, FALSE, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE, MAX_PATH,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING,
};
use windows_sys::Win32::System::Registry::{REG_MULTI_SZ, REG_SZ};
use windows_sys::core::GUID;

use crate::platform::device::{
    UsbPnpDevice, UsbProblemDevice, UsbResetInterface, is_picotool_reset_compatible_id,
};
use crate::usb::{UsbRecoveryHealth, UNCLASSED_DEVICE_CLASS};

use super::device::{ancestor_ids, as_utf16, location_paths_from_info, parse_usb_port_info};

const PICO_RESET_INTERFACE_GUID: GUID = GUID::from_u128(0xbc7398c1_73cd_4cb7_98b8_913a8fca7bf6);
const RESET_REQUEST_BOOTSEL: u8 = 0x01;
// USB_DIR_OUT | USB_TYPE_CLASS | USB_RECIP_INTERFACE. This exactly
// matches picotool's reset-interface request; the endpoint is vendor
// class, but the control request itself is class-scoped.
const RESET_REQUEST_TYPE: u8 = 0x21;

pub(crate) fn present_usb_problem_devices() -> Vec<UsbProblemDevice> {
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

pub(crate) fn present_usb_reset_interfaces() -> Vec<UsbResetInterface> {
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
        let Some(identity) = parse_usb_port_info(&instance_id, Some(&parent_instance_id)) else {
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

pub(crate) fn reset_usb_interface_to_bootsel(interface: &UsbResetInterface) -> io::Result<()> {
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

pub(crate) fn inspect_usb_pnp_device(
    instance_id: &str,
    allow_phantom: bool,
) -> Result<UsbPnpDevice, String> {
    let devinst = locate(instance_id, allow_phantom)?;
    let actual_instance_id = device_id(devinst)?;
    let parent_instance_id = parent_id(devinst)?;
    let device_class = device_class(devinst)?;
    let health = device_health(devinst);
    let location_paths = device_location_paths(devinst);
    let (vid, pid, serial) =
        parse_usb_identity(&actual_instance_id, parent_instance_id.as_deref()).ok_or_else(
            || "device does not expose a canonical USB VID/PID identity".to_string(),
        )?;

    Ok(UsbPnpDevice {
        instance_id: actual_instance_id,
        parent_instance_id,
        device_class,
        vid,
        pid,
        serial,
        health,
        location_paths,
    })
}

pub(crate) fn reenumerate_usb_parent(parent_instance_id: &str) -> Result<(), String> {
    let parent = locate(parent_instance_id, false)?;
    // SAFETY: `parent` was obtained from Config Manager for the exact
    // verified live parent. Flags are zero, requesting no broad scan.
    let result = unsafe { CM_Reenumerate_DevNode(parent, 0) };
    (result == CR_SUCCESS)
        .then_some(())
        .ok_or_else(|| format!("CM_Reenumerate_DevNode failed ({result})"))
}

pub(crate) fn restart_usb_device(instance_id: &str) -> Result<(), String> {
    let target = locate(instance_id, false)?;
    // SAFETY: `target` was revalidated as the exact present problematic
    // child by the recovery ladder. The helper never passes a
    // parent/hub/controller to this call.
    let disabled = unsafe { CM_Disable_DevNode(target, 0) };
    if disabled != CR_SUCCESS {
        return Err(format!("CM_Disable_DevNode failed ({disabled})"));
    }
    // SAFETY: same exact child devinst as the immediately preceding
    // disable. No other Config Manager action is performed here.
    let enabled = unsafe { CM_Enable_DevNode(target, 0) };
    if enabled == CR_SUCCESS {
        return Ok(());
    }
    // Best-effort rollback for a transient Config Manager failure. This
    // is still the same exact child and does not widen the allowlist; its
    // result is retained in the diagnostic rather than silently leaving a
    // potentially disabled endpoint behind.
    // SAFETY: same exact validated child devinst; this is a bounded
    // best-effort re-enable after the first enable reported failure.
    let rollback = unsafe { CM_Enable_DevNode(target, 0) };
    Err(format!(
        "CM_Enable_DevNode failed ({enabled}); rollback enable returned ({rollback})"
    ))
}

pub(crate) fn usb_pnp_post_operation_poll_attempts() -> usize {
    8
}

pub(crate) fn usb_pnp_post_operation_poll_interval() -> Duration {
    Duration::from_millis(250)
}

fn from_utf16(buffer: &[u16]) -> String {
    let length = buffer
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..length])
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

fn locate(instance_id: &str, allow_phantom: bool) -> Result<u32, String> {
    let mut devinst = 0u32;
    let utf16 = instance_id
        .encode_utf16()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let flags = if allow_phantom {
        CM_LOCATE_DEVNODE_PHANTOM
    } else {
        CM_LOCATE_DEVNODE_NORMAL
    };
    // SAFETY: `utf16` is NUL-terminated and remains alive for the call;
    // `devinst` is writable local storage.
    let result = unsafe { CM_Locate_DevNodeW(&mut devinst, utf16.as_ptr(), flags) };
    (result == CR_SUCCESS)
        .then_some(devinst)
        .ok_or_else(|| format!("CM_Locate_DevNodeW failed ({result})"))
}

fn device_id(devinst: u32) -> Result<String, String> {
    let mut buffer = [0u16; MAX_DEVICE_ID_LEN as usize];
    // SAFETY: `buffer` is writable local UTF-16 storage sized according to
    // the Config Manager API's documented maximum device ID length.
    let result = unsafe {
        CM_Get_Device_IDW(devinst, buffer.as_mut_ptr(), (buffer.len() - 1) as u32, 0)
    };
    if result != CR_SUCCESS {
        return Err(format!("CM_Get_Device_IDW failed ({result})"));
    }
    Ok(from_utf16(&buffer))
}

fn parent_id(devinst: u32) -> Result<Option<String>, String> {
    let mut parent = 0u32;
    // SAFETY: `parent` is writable local storage and `devinst` came from
    // Config Manager in the same process.
    let result = unsafe { CM_Get_Parent(&mut parent, devinst, 0) };
    if result == CR_NO_SUCH_DEVINST {
        return Ok(None);
    }
    if result != CR_SUCCESS {
        return Err(format!("CM_Get_Parent failed ({result})"));
    }
    device_id(parent).map(Some)
}

fn device_class(devinst: u32) -> Result<String, String> {
    let mut property_type = 0u32;
    let mut buffer = [0u16; 256];
    let mut byte_len = (buffer.len() * std::mem::size_of::<u16>()) as u32;
    // SAFETY: the property key and all output pointers remain valid for
    // the call; the buffer size is provided in bytes as required by CM.
    let result = unsafe {
        CM_Get_DevNode_PropertyW(
            devinst,
            &DEVPKEY_Device_Class,
            &mut property_type,
            buffer.as_mut_ptr().cast(),
            &mut byte_len,
            0,
        )
    };
    if result == CR_NO_SUCH_VALUE {
        // Driverless devnodes (e.g. a BOOTSEL PICOBOOT interface stuck at
        // CM_PROB_FAILED_INSTALL) have no Device_Class property at all.
        // Report the shared sentinel so identity revalidation treats the
        // absence as an exact-match fact (FastLED/fbuild#1152).
        return Ok(UNCLASSED_DEVICE_CLASS.to_string());
    }
    if result != CR_SUCCESS || property_type != DEVPROP_TYPE_STRING {
        return Err(format!(
            "CM_Get_DevNode_PropertyW(Device_Class) failed ({result})"
        ));
    }
    Ok(from_utf16(&buffer))
}

fn device_health(devinst: u32) -> UsbRecoveryHealth {
    let mut status = 0u32;
    let mut problem_code = 0u32;
    // SAFETY: both output pointers are writable local storage and the
    // devinst was returned by Config Manager.
    let result = unsafe { CM_Get_DevNode_Status(&mut status, &mut problem_code, devinst, 0) };
    if result == CR_SUCCESS {
        if problem_code == 0 {
            UsbRecoveryHealth::HealthyPresent
        } else {
            UsbRecoveryHealth::PresentProblem { problem_code }
        }
    } else if result == CR_NO_SUCH_DEVINST {
        UsbRecoveryHealth::Phantom { problem_code: None }
    } else {
        UsbRecoveryHealth::Unknown
    }
}

fn device_location_paths(devinst: u32) -> Vec<String> {
    let mut property_type = 0u32;
    let mut byte_len = 0u32;
    unsafe {
        CM_Get_DevNode_PropertyW(
            devinst,
            &DEVPKEY_Device_LocationPaths,
            &mut property_type,
            std::ptr::null_mut(),
            &mut byte_len,
            0,
        )
    };
    if byte_len < 2 {
        return Vec::new();
    }
    let mut buffer = vec![0u16; (byte_len as usize).div_ceil(2)];
    let result = unsafe {
        CM_Get_DevNode_PropertyW(
            devinst,
            &DEVPKEY_Device_LocationPaths,
            &mut property_type,
            buffer.as_mut_ptr().cast(),
            &mut byte_len,
            0,
        )
    };
    if result != CR_SUCCESS || property_type != DEVPROP_TYPE_STRING_LIST {
        return Vec::new();
    }
    buffer
        .split(|unit| *unit == 0)
        .take_while(|segment| !segment.is_empty())
        .map(String::from_utf16_lossy)
        .filter(|path| !path.is_empty())
        .collect()
}

fn parse_usb_identity(
    instance_id: &str,
    parent_instance_id: Option<&str>,
) -> Option<(u16, u16, Option<String>)> {
    fn parse(id: &str) -> Option<(u16, u16, Option<String>)> {
        let mut parts = id.split('\\');
        if !parts.next()?.eq_ignore_ascii_case("USB") {
            return None;
        }
        let hardware = parts.next()?.to_ascii_uppercase();
        let vid_start = hardware.find("VID_")? + 4;
        let pid_start = hardware.find("PID_")? + 4;
        let vid = u16::from_str_radix(hardware.get(vid_start..vid_start + 4)?, 16).ok()?;
        let pid = u16::from_str_radix(hardware.get(pid_start..pid_start + 4)?, 16).ok()?;
        let serial = parts
            .next()
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        Some((vid, pid, serial))
    }

    let (vid, pid, child_serial) = parse(instance_id)?;
    let parent_serial =
        parent_instance_id
            .and_then(parse)
            .and_then(|(parent_vid, parent_pid, serial)| {
                (parent_vid == vid && parent_pid == pid)
                    .then_some(serial)
                    .flatten()
            });
    Some((vid, pid, parent_serial.or(child_serial)))
}
