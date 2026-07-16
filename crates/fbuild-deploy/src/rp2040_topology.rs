//! USB topology capture for RP-series deploy diagnostics (Windows only) and
//! the removable-drive predicate used to keep the BOOTSEL volume scan off
//! dead mapped network drives (FastLED/fbuild#1081, #1082).

/// True when `root` (e.g. `D:\`) is a `DRIVE_REMOVABLE` volume per
/// `GetDriveTypeW`. Used to keep the default Windows drive-letter scan from
/// touching a disconnected mapped network drive or a fixed disk.
#[cfg(windows)]
pub(super) fn is_removable_drive(root: &std::path::Path) -> bool {
    windows_impl::is_removable_drive(root)
}

/// One-line human-readable USB topology for a runtime COM port, or `None`
/// when the platform or host cannot supply one. Never panics: every FFI
/// failure degrades to `None` rather than guessing at topology.
#[cfg(windows)]
pub(super) fn describe_port_topology(port_name: &str) -> Option<String> {
    windows_impl::describe_port_topology(port_name)
}

#[cfg(not(windows))]
pub(super) fn describe_port_topology(_port_name: &str) -> Option<String> {
    None
}

/// Coarse USB hub-tier classification derived from an ancestor
/// instance-ID chain (child -> root order; the device's own ID is not
/// included). Kept separate from the FFI walk so it is deterministically
/// testable on every host OS.
#[cfg(any(windows, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HubDepth {
    DirectRootPort,
    BehindHubs(usize),
    /// The chain was empty, or didn't resolve to a recognizable
    /// hub/root-hub pattern before the ancestor walk gave up. Distinct
    /// from a genuine zero-hub result -- never guessed.
    Unavailable,
}

#[cfg(any(windows, test))]
pub(super) fn classify_ancestor_chain(own_id: &str, ancestor_ids: &[String]) -> HubDepth {
    // A composite-device CDC function enumerates as an interface node
    // (`USB\VID_xxxx&PID_yyyy&MI_00\...`) whose leading ancestor is the USB
    // *device* node carrying the same VID&PID token — not a hub. Counting it
    // would over-report depth by one on every composite-CDC board, so drop
    // leading same-device ancestors before counting hub tiers.
    let own_token = vid_pid_token(own_id);
    let ancestor_ids = match &own_token {
        Some(token) => {
            let same_device = ancestor_ids
                .iter()
                .take_while(|id| id.to_ascii_uppercase().contains(token))
                .count();
            &ancestor_ids[same_device..]
        }
        None => ancestor_ids,
    };
    if ancestor_ids.is_empty() {
        return HubDepth::Unavailable;
    }
    if is_root_hub_id(&ancestor_ids[0]) {
        return HubDepth::DirectRootPort;
    }
    for (hub_tiers, id) in ancestor_ids.iter().enumerate() {
        if is_root_hub_id(id) {
            return HubDepth::BehindHubs(hub_tiers);
        }
        if !is_usb_device_id(id) {
            // Not a hub-shaped ancestor and not a root hub either: stop
            // rather than guess how many tiers remain.
            return HubDepth::Unavailable;
        }
    }
    // Walked off the end of the collected chain without reaching a root hub.
    HubDepth::Unavailable
}

/// Extract the uppercase `VID_xxxx&PID_yyyy` token from a USB instance ID,
/// dropping any interface suffix (`&MI_nn`). `None` for non-USB IDs.
#[cfg(any(windows, test))]
fn vid_pid_token(instance_id: &str) -> Option<String> {
    let upper = instance_id.to_ascii_uppercase();
    let device_part = upper.strip_prefix("USB\\")?.split('\\').next()?;
    let mut fields = device_part.split('&');
    let vid = fields.next().filter(|field| field.starts_with("VID_"))?;
    let pid = fields.next().filter(|field| field.starts_with("PID_"))?;
    Some(format!("{vid}&{pid}"))
}

#[cfg(any(windows, test))]
fn is_root_hub_id(instance_id: &str) -> bool {
    instance_id.to_ascii_uppercase().starts_with("USB\\ROOT_HUB")
}

#[cfg(any(windows, test))]
fn is_usb_device_id(instance_id: &str) -> bool {
    let upper = instance_id.to_ascii_uppercase();
    upper.starts_with("USB\\VID_") && upper.contains("&PID_")
}

/// Render a [`HubDepth`] plus an optional Windows location string
/// (`SPDRP_LOCATION_INFORMATION`, e.g. `Port_#0003.Hub_#0004`) into the
/// one-line summary appended to deploy failure messages. Facts fbuild
/// cannot query (hub power mode, sibling count) are labeled explicitly
/// rather than omitted or guessed.
#[cfg(any(windows, test))]
pub(super) fn format_topology(depth: HubDepth, location: Option<&str>) -> String {
    match depth {
        HubDepth::DirectRootPort => match location {
            Some(location) => format!("USB topology: direct root port ({location})"),
            None => "USB topology: direct root port".to_string(),
        },
        HubDepth::BehindHubs(tiers) => {
            let at = match location {
                Some(location) => format!(" at {location}"),
                None => String::new(),
            };
            format!(
                "USB topology: behind {tiers} external hub tier(s){at}; hub power mode and sibling count unavailable (not queried)"
            )
        }
        HubDepth::Unavailable => "USB topology unavailable".to_string(),
    }
}

#[cfg(windows)]
mod windows_impl {
    use std::ffi::{c_void, OsStr};
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use super::{classify_ancestor_chain, format_topology};

    const DIGCF_PRESENT: u32 = 0x0000_0002;
    const DICS_FLAG_GLOBAL: u32 = 0x0000_0001;
    const DIREG_DEV: u32 = 0x0000_0001;
    const KEY_READ: u32 = 0x0002_0019;
    const REG_SZ: u32 = 1;
    const ERROR_SUCCESS: i32 = 0;
    const SPDRP_LOCATION_INFORMATION: u32 = 0x0000_000D;
    const CR_SUCCESS: u32 = 0;
    const DRIVE_REMOVABLE: u32 = 2;
    // Real USB hub trees are at most a handful of tiers deep; this bounds a
    // wedged/cyclic CM_Get_Parent walk instead of looping forever.
    const MAX_ANCESTOR_DEPTH: usize = 16;
    const DEVICE_ID_BUFFER_LEN: usize = 256;

    #[repr(C)]
    struct Guid {
        data1: u32,
        data2: u16,
        data3: u16,
        data4: [u8; 8],
    }

    // {4D36E978-E325-11CE-BFC1-08002BE10318} -- GUID_DEVCLASS_PORTS.
    const GUID_DEVCLASS_PORTS: Guid = Guid {
        data1: 0x4D36_E978,
        data2: 0xE325,
        data3: 0x11CE,
        data4: [0xBF, 0xC1, 0x08, 0x00, 0x2B, 0xE1, 0x03, 0x18],
    };

    #[repr(C)]
    struct SpDevinfoData {
        cb_size: u32,
        class_guid: Guid,
        dev_inst: u32,
        reserved: usize,
    }

    #[link(name = "setupapi")]
    extern "system" {
        fn SetupDiGetClassDevsW(
            class_guid: *const Guid,
            enumerator: *const u16,
            hwnd_parent: *mut c_void,
            flags: u32,
        ) -> *mut c_void;
        fn SetupDiEnumDeviceInfo(
            device_info_set: *mut c_void,
            member_index: u32,
            device_info_data: *mut SpDevinfoData,
        ) -> i32;
        fn SetupDiOpenDevRegKey(
            device_info_set: *mut c_void,
            device_info_data: *mut SpDevinfoData,
            scope: u32,
            hw_profile: u32,
            key_type: u32,
            sam_desired: u32,
        ) -> *mut c_void;
        fn SetupDiGetDeviceRegistryPropertyW(
            device_info_set: *mut c_void,
            device_info_data: *mut SpDevinfoData,
            property: u32,
            property_reg_data_type: *mut u32,
            property_buffer: *mut u8,
            property_buffer_size: u32,
            required_size: *mut u32,
        ) -> i32;
        fn SetupDiDestroyDeviceInfoList(device_info_set: *mut c_void) -> i32;
    }

    #[link(name = "advapi32")]
    extern "system" {
        fn RegQueryValueExW(
            hkey: *mut c_void,
            value_name: *const u16,
            reserved: *mut u32,
            data_type: *mut u32,
            data: *mut u8,
            data_size: *mut u32,
        ) -> i32;
        fn RegCloseKey(hkey: *mut c_void) -> i32;
    }

    #[link(name = "cfgmgr32")]
    extern "system" {
        fn CM_Get_Parent(parent_devinst: *mut u32, devinst: u32, flags: u32) -> u32;
        fn CM_Get_Device_IDW(devinst: u32, buffer: *mut u16, buffer_len: u32, flags: u32) -> u32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetDriveTypeW(root_path_name: *const u16) -> u32;
    }

    fn to_wide(value: &str) -> Vec<u16> {
        OsStr::new(value)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    fn from_wide_lossy(buffer: &[u16]) -> String {
        let len = buffer
            .iter()
            .position(|&unit| unit == 0)
            .unwrap_or(buffer.len());
        String::from_utf16_lossy(&buffer[..len])
    }

    /// `SetupDiGetClassDevsW`/`SetupDiOpenDevRegKey` report failure as
    /// `INVALID_HANDLE_VALUE` (all bits set), not always a null pointer.
    fn is_invalid_handle(handle: *mut c_void) -> bool {
        handle.is_null() || handle as usize == usize::MAX
    }

    pub(super) fn is_removable_drive(root: &Path) -> bool {
        let wide_root = to_wide(&root.to_string_lossy());
        unsafe { GetDriveTypeW(wide_root.as_ptr()) == DRIVE_REMOVABLE }
    }

    fn new_devinfo_data() -> SpDevinfoData {
        SpDevinfoData {
            cb_size: std::mem::size_of::<SpDevinfoData>() as u32,
            class_guid: Guid {
                data1: 0,
                data2: 0,
                data3: 0,
                data4: [0; 8],
            },
            dev_inst: 0,
            reserved: 0,
        }
    }

    fn read_port_name(hdevinfo: *mut c_void, info: &mut SpDevinfoData) -> Option<String> {
        let hkey = unsafe {
            SetupDiOpenDevRegKey(hdevinfo, info, DICS_FLAG_GLOBAL, 0, DIREG_DEV, KEY_READ)
        };
        if is_invalid_handle(hkey) {
            return None;
        }
        let value_name = to_wide("PortName");
        let mut buffer = [0u8; DEVICE_ID_BUFFER_LEN];
        let mut size = buffer.len() as u32;
        let mut data_type = 0u32;
        let status = unsafe {
            RegQueryValueExW(
                hkey,
                value_name.as_ptr(),
                std::ptr::null_mut(),
                &mut data_type,
                buffer.as_mut_ptr(),
                &mut size,
            )
        };
        unsafe {
            RegCloseKey(hkey);
        }
        if status != ERROR_SUCCESS || data_type != REG_SZ {
            return None;
        }
        let word_count = ((size as usize) / 2).min(buffer.len() / 2);
        let wide: Vec<u16> = buffer[..word_count * 2]
            .chunks_exact(2)
            .map(|pair| u16::from_ne_bytes([pair[0], pair[1]]))
            .collect();
        Some(from_wide_lossy(&wide))
    }

    fn read_location(hdevinfo: *mut c_void, info: &mut SpDevinfoData) -> Option<String> {
        let mut buffer = [0u16; DEVICE_ID_BUFFER_LEN];
        let ok = unsafe {
            SetupDiGetDeviceRegistryPropertyW(
                hdevinfo,
                info,
                SPDRP_LOCATION_INFORMATION,
                std::ptr::null_mut(),
                buffer.as_mut_ptr() as *mut u8,
                (buffer.len() * 2) as u32,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return None;
        }
        let text = from_wide_lossy(&buffer);
        (!text.is_empty()).then_some(text)
    }

    fn find_devinst_for_port(port_name: &str) -> Option<(u32, Option<String>)> {
        let hdevinfo = unsafe {
            SetupDiGetClassDevsW(
                &GUID_DEVCLASS_PORTS,
                std::ptr::null(),
                std::ptr::null_mut(),
                DIGCF_PRESENT,
            )
        };
        if is_invalid_handle(hdevinfo) {
            return None;
        }
        let mut index = 0u32;
        let found = loop {
            let mut info = new_devinfo_data();
            let ok = unsafe { SetupDiEnumDeviceInfo(hdevinfo, index, &mut info) };
            if ok == 0 {
                break None;
            }
            index += 1;
            let Some(candidate_name) = read_port_name(hdevinfo, &mut info) else {
                continue;
            };
            if candidate_name.eq_ignore_ascii_case(port_name) {
                let location = read_location(hdevinfo, &mut info);
                break Some((info.dev_inst, location));
            }
        };
        unsafe {
            SetupDiDestroyDeviceInfoList(hdevinfo);
        }
        found
    }

    fn device_instance_id(devinst: u32) -> Option<String> {
        let mut buffer = [0u16; DEVICE_ID_BUFFER_LEN];
        if unsafe { CM_Get_Device_IDW(devinst, buffer.as_mut_ptr(), buffer.len() as u32, 0) }
            != CR_SUCCESS
        {
            return None;
        }
        Some(from_wide_lossy(&buffer))
    }

    fn ancestor_chain(devinst: u32) -> Vec<String> {
        let mut ids = Vec::new();
        let mut current = devinst;
        for _ in 0..MAX_ANCESTOR_DEPTH {
            let mut parent = 0u32;
            if unsafe { CM_Get_Parent(&mut parent, current, 0) } != CR_SUCCESS {
                break;
            }
            let Some(id) = device_instance_id(parent) else {
                break;
            };
            ids.push(id);
            current = parent;
        }
        ids
    }

    pub(super) fn describe_port_topology(port_name: &str) -> Option<String> {
        let (devinst, location) = find_devinst_for_port(port_name)?;
        // An unreadable own ID degrades to no composite-ancestor skipping,
        // not to a lost topology line.
        let own_id = device_instance_id(devinst).unwrap_or_default();
        let depth = classify_ancestor_chain(&own_id, &ancestor_chain(devinst));
        Some(format_topology(depth, location.as_deref()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Synthetic non-composite own ID: no `&MI_nn` interface suffix, so its
    // VID&PID token never matches the hub ancestors below.
    const OWN_ID: &str = "USB\\VID_9999&PID_8888\\5303284720C4641C";

    #[test]
    fn direct_root_port_is_the_immediate_parent() {
        let ids = vec!["USB\\ROOT_HUB30\\4&1a2b3c4d&0".to_string()];
        assert_eq!(
            classify_ancestor_chain(OWN_ID, &ids),
            HubDepth::DirectRootPort
        );
    }

    #[test]
    fn single_external_hub_tier_is_counted() {
        let ids = vec![
            "USB\\VID_1111&PID_2222\\5&AABB".to_string(),
            "USB\\ROOT_HUB30\\4&1a2b3c4d&0".to_string(),
        ];
        assert_eq!(
            classify_ancestor_chain(OWN_ID, &ids),
            HubDepth::BehindHubs(1)
        );
    }

    #[test]
    fn hub_on_hub_counts_every_tier() {
        let ids = vec![
            "USB\\VID_1111&PID_2222\\5&AABB".to_string(),
            "USB\\VID_3333&PID_4444\\6&CCDD".to_string(),
            "USB\\VID_5555&PID_6666\\7&EEFF".to_string(),
            "USB\\ROOT_HUB30\\4&1a2b3c4d&0".to_string(),
        ];
        assert_eq!(
            classify_ancestor_chain(OWN_ID, &ids),
            HubDepth::BehindHubs(3)
        );
    }

    #[test]
    fn empty_or_unrecognizable_chain_is_unavailable() {
        assert_eq!(classify_ancestor_chain(OWN_ID, &[]), HubDepth::Unavailable);
        let garbage = vec!["PCI\\VEN_8086&DEV_1234\\3&11583659&0&D8".to_string()];
        assert_eq!(
            classify_ancestor_chain(OWN_ID, &garbage),
            HubDepth::Unavailable
        );
    }

    #[test]
    fn chain_that_never_reaches_a_root_hub_is_unavailable() {
        let ids = vec!["USB\\VID_1111&PID_2222\\5&AABB".to_string()];
        assert_eq!(
            classify_ancestor_chain(OWN_ID, &ids),
            HubDepth::Unavailable
        );
    }

    #[test]
    fn composite_device_node_is_not_counted_as_a_hub_tier() {
        let own_id = "USB\\VID_9999&PID_8888&MI_00\\7&99&0000";
        let ids = vec![
            "USB\\VID_9999&PID_8888\\5303284720C4641C".to_string(),
            "USB\\VID_1111&PID_2222\\5&AABB".to_string(),
            "USB\\ROOT_HUB30\\4&1a2b3c4d&0".to_string(),
        ];
        assert_eq!(
            classify_ancestor_chain(own_id, &ids),
            HubDepth::BehindHubs(1)
        );
    }

    #[test]
    fn composite_device_on_root_port_is_direct() {
        let own_id = "USB\\VID_9999&PID_8888&MI_00\\7&99&0000";
        let ids = vec![
            "USB\\VID_9999&PID_8888\\5303284720C4641C".to_string(),
            "USB\\ROOT_HUB30\\4&1a2b3c4d&0".to_string(),
        ];
        assert_eq!(
            classify_ancestor_chain(own_id, &ids),
            HubDepth::DirectRootPort
        );
    }

    #[test]
    fn own_id_without_vid_pid_token_skips_nothing() {
        let ids = vec![
            "USB\\VID_1111&PID_2222\\5&AABB".to_string(),
            "USB\\ROOT_HUB30\\4&1a2b3c4d&0".to_string(),
        ];
        assert_eq!(
            classify_ancestor_chain("FTDIBUS\\COMPORT&VID_0403", &ids),
            HubDepth::BehindHubs(1)
        );
        assert_eq!(classify_ancestor_chain("", &ids), HubDepth::BehindHubs(1));
    }

    #[test]
    fn direct_root_port_formats_with_and_without_location() {
        assert_eq!(
            format_topology(HubDepth::DirectRootPort, Some("Port_#0001.Hub_#0002")),
            "USB topology: direct root port (Port_#0001.Hub_#0002)"
        );
        assert_eq!(
            format_topology(HubDepth::DirectRootPort, None),
            "USB topology: direct root port"
        );
    }

    #[test]
    fn behind_hubs_labels_unqueried_facts_explicitly() {
        assert_eq!(
            format_topology(HubDepth::BehindHubs(2), Some("Port_#0003.Hub_#0004")),
            "USB topology: behind 2 external hub tier(s) at Port_#0003.Hub_#0004; hub power mode and sibling count unavailable (not queried)"
        );
        assert_eq!(
            format_topology(HubDepth::BehindHubs(1), None),
            "USB topology: behind 1 external hub tier(s); hub power mode and sibling count unavailable (not queried)"
        );
    }

    #[test]
    fn unavailable_chain_yields_the_flat_fallback_message() {
        assert_eq!(
            format_topology(HubDepth::Unavailable, Some("Port_#0001.Hub_#0002")),
            "USB topology unavailable"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn non_windows_never_reports_topology() {
        assert_eq!(describe_port_topology("COM12"), None);
    }
}
