//! Strict, one-shot Windows USB PnP recovery boundary.
//!
//! The elevated CLI helper is intentionally thin: it supplies a typed request
//! and calls this module. This module can express only two operations, both
//! after an authoritative identity re-query: re-enumerate the exact live
//! parent of a phantom target, or restart the exact present problematic child.
//! It never opens a serial port, touches the fbuild daemon, or returns a COM
//! port. FastLED/fbuild#1148.

use fbuild_core::usb::{
    UsbRecoveryHealth, UsbRecoveryOperation, UsbRecoveryRequest, UsbRecoveryResult,
};

/// A PnP devnode observed directly by the recovery backend.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UsbPnpDevice {
    pub instance_id: String,
    pub parent_instance_id: Option<String>,
    pub device_class: String,
    pub vid: u16,
    pub pid: u16,
    pub serial: Option<String>,
    pub health: UsbRecoveryHealth,
}

/// Narrow host boundary used by the elevated helper and deterministic tests.
///
/// No operation accepts a command line, arbitrary program, registry path, or
/// broad device selector. The caller passes only a canonical instance ID that
/// was revalidated by [`execute_recovery`].
pub trait UsbPnpBackend {
    type Error: std::fmt::Display;

    /// Return the current node. `allow_phantom` is required only to inspect
    /// the original target; verified parents are always looked up live.
    fn inspect(
        &mut self,
        instance_id: &str,
        allow_phantom: bool,
    ) -> Result<UsbPnpDevice, Self::Error>;

    /// Re-enumerate only the exact, verified live parent of a phantom target.
    fn reenumerate_parent(&mut self, parent_instance_id: &str) -> Result<(), Self::Error>;

    /// Restart only the exact, verified present target child.
    fn restart_target(&mut self, instance_id: &str) -> Result<(), Self::Error>;

    /// Restart only the exact, verified healthy parent composite of a
    /// present problematic USB interface devnode (FastLED/fbuild#1152).
    fn restart_verified_parent(&mut self, parent_instance_id: &str) -> Result<(), Self::Error>;

    /// Number of bounded post-operation observations. Fakes stay instant;
    /// Windows waits briefly between observations for re-enumeration to settle.
    fn post_operation_poll_attempts(&self) -> usize {
        1
    }

    fn wait_for_post_operation_poll(&mut self) {}
}

/// Execute the bounded recovery ladder with a real or fake PnP backend.
///
/// A successful result means the allowlisted PnP operation completed, not that
/// the device is deployable. The normal unprivileged process must still run a
/// fresh #1146 health/openability probe before it can choose a serial port.
pub fn execute_recovery<B: UsbPnpBackend>(
    request: &UsbRecoveryRequest,
    nonce: String,
    backend: &mut B,
) -> UsbRecoveryResult {
    let failed = |before: UsbRecoveryHealth, error_code: &str| UsbRecoveryResult {
        operation_id: request.operation_id.clone(),
        nonce: nonce.clone(),
        validated_instance_id: None,
        operation: None,
        before: before.clone(),
        after: before,
        success: false,
        error_code: Some(error_code.to_string()),
    };

    if !request.has_canonical_identity() {
        return failed(UsbRecoveryHealth::Unknown, "invalid-request-identity");
    }

    let target = match backend.inspect(&request.instance_id, true) {
        Ok(device) => device,
        Err(_) => return failed(UsbRecoveryHealth::Unknown, "target-not-found"),
    };
    let before = target.health.clone();
    if let Err(error_code) = validate_target_identity(request, &target) {
        return failed(before, error_code);
    }

    let (operation, action_result) = match target.health {
        UsbRecoveryHealth::Phantom { .. } => {
            let Some(parent_instance_id) = request.parent_instance_id.as_deref() else {
                return failed(before, "missing-verified-parent");
            };
            let parent = match backend.inspect(parent_instance_id, false) {
                Ok(device) => device,
                Err(_) => return failed(before, "parent-not-live"),
            };
            if !matches!(parent.health, UsbRecoveryHealth::HealthyPresent)
                || !same_id(&parent.instance_id, parent_instance_id)
            {
                return failed(before, "parent-not-live");
            }
            (
                UsbRecoveryOperation::ReenumerateParent,
                backend.reenumerate_parent(parent_instance_id),
            )
        }
        // A composite-interface devnode (`...&MI_xx\...`) cannot recover
        // alone: restarting it leaves the sibling interfaces and any mounted
        // synthetic volume (e.g. the RP2040 BOOTSEL FAT) in their wedged
        // state. When the request names a parent, restart the live-verified
        // healthy parent composite instead (FastLED/fbuild#1152). A plain
        // device target keeps the original exact-child restart.
        UsbRecoveryHealth::PresentProblem { .. } if is_composite_interface(&target.instance_id) => {
            let Some(parent_instance_id) = request.parent_instance_id.as_deref() else {
                return failed(before, "missing-verified-parent");
            };
            let parent = match backend.inspect(parent_instance_id, false) {
                Ok(device) => device,
                Err(_) => return failed(before, "parent-not-live"),
            };
            if !matches!(parent.health, UsbRecoveryHealth::HealthyPresent)
                || !same_id(&parent.instance_id, parent_instance_id)
                || parent.vid != target.vid
                || parent.pid != target.pid
            {
                return failed(before, "parent-not-live");
            }
            (
                UsbRecoveryOperation::RestartVerifiedParent,
                backend.restart_verified_parent(parent_instance_id),
            )
        }
        UsbRecoveryHealth::PresentProblem { .. } => (
            UsbRecoveryOperation::RestartTarget,
            backend.restart_target(&target.instance_id),
        ),
        UsbRecoveryHealth::HealthyPresent => return failed(before, "target-already-healthy"),
        UsbRecoveryHealth::Unknown => return failed(before, "target-health-unknown"),
    };

    if action_result.is_err() {
        return failed(before, "pnp-operation-failed");
    }

    let mut after = UsbRecoveryHealth::Unknown;
    for _ in 0..backend.post_operation_poll_attempts().max(1) {
        backend.wait_for_post_operation_poll();
        after = backend
            .inspect(&request.instance_id, true)
            .map(|device| device.health)
            .unwrap_or(UsbRecoveryHealth::Unknown);
        if matches!(after, UsbRecoveryHealth::HealthyPresent) {
            break;
        }
    }
    UsbRecoveryResult {
        operation_id: request.operation_id.clone(),
        nonce,
        validated_instance_id: Some(target.instance_id),
        operation: Some(operation),
        before,
        after,
        success: true,
        error_code: None,
    }
}

fn validate_target_identity(
    request: &UsbRecoveryRequest,
    target: &UsbPnpDevice,
) -> Result<(), &'static str> {
    if !same_id(&target.instance_id, &request.instance_id) {
        return Err("instance-id-mismatch");
    }
    if !target
        .device_class
        .eq_ignore_ascii_case(&request.expected_class)
    {
        return Err("device-class-mismatch");
    }
    if target.vid != request.expected_vid || target.pid != request.expected_pid {
        return Err("vid-pid-mismatch");
    }
    if let Some(expected_serial) = request.expected_serial.as_deref() {
        if target.serial.as_deref() != Some(expected_serial) {
            return Err("serial-mismatch");
        }
    }
    if let (Some(expected_problem_code), UsbRecoveryHealth::PresentProblem { problem_code }) =
        (request.problem_code, &target.health)
    {
        if *problem_code != expected_problem_code {
            return Err("problem-code-mismatch");
        }
    }
    if let Some(expected_parent) = request.parent_instance_id.as_deref() {
        match target.parent_instance_id.as_deref() {
            Some(actual_parent) if !same_id(actual_parent, expected_parent) => {
                return Err("parent-mismatch");
            }
            // A phantom can be recovered only if Config Manager still proves
            // its immediate parent. The parent ID from the normal unprivileged
            // request alone is never authority to touch a live USB node.
            None => {
                return Err("parent-mismatch");
            }
            _ => {}
        }
    }
    Ok(())
}

fn same_id(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

/// Whether the instance is a USB composite-interface devnode (`usbccgp`
/// child), recognizable by the `&MI_xx` hardware-ID component.
fn is_composite_interface(instance_id: &str) -> bool {
    instance_id.to_ascii_uppercase().contains("&MI_")
}

/// Perform real host recovery when the one-shot helper is running on Windows.
///
/// The non-Windows result deliberately fails closed. The CLI must render the
/// physical recovery instructions instead of attempting any platform-specific
/// substitute.
pub fn recover_windows_usb_device(
    request: &UsbRecoveryRequest,
    nonce: String,
) -> UsbRecoveryResult {
    #[cfg(windows)]
    {
        let mut backend = windows::WindowsPnpBackend;
        execute_recovery(request, nonce, &mut backend)
    }
    #[cfg(not(windows))]
    {
        UsbRecoveryResult {
            operation_id: request.operation_id.clone(),
            nonce,
            validated_instance_id: None,
            operation: None,
            before: UsbRecoveryHealth::Unknown,
            after: UsbRecoveryHealth::Unknown,
            success: false,
            error_code: Some("windows-recovery-unavailable".to_string()),
        }
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::time::Duration;
    use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
        CM_Disable_DevNode, CM_Enable_DevNode, CM_Get_DevNode_PropertyW, CM_Get_DevNode_Status,
        CM_Get_Device_IDW, CM_Get_Parent, CM_LOCATE_DEVNODE_NORMAL, CM_LOCATE_DEVNODE_PHANTOM,
        CM_Locate_DevNodeW, CM_Reenumerate_DevNode, CR_NO_SUCH_DEVINST, CR_NO_SUCH_VALUE,
        CR_SUCCESS, MAX_DEVICE_ID_LEN,
    };
    use windows_sys::Win32::Devices::Properties::{DEVPKEY_Device_Class, DEVPROP_TYPE_STRING};

    /// Windows implementation is added below the common security ladder so
    /// tests can exercise every allowlist decision without a privileged host.
    pub(super) struct WindowsPnpBackend;

    impl UsbPnpBackend for WindowsPnpBackend {
        type Error = String;

        fn inspect(
            &mut self,
            instance_id: &str,
            allow_phantom: bool,
        ) -> Result<UsbPnpDevice, Self::Error> {
            inspect_device(instance_id, allow_phantom)
        }

        fn reenumerate_parent(&mut self, parent_instance_id: &str) -> Result<(), Self::Error> {
            reenumerate_parent(parent_instance_id)
        }

        fn restart_target(&mut self, instance_id: &str) -> Result<(), Self::Error> {
            restart_target(instance_id)
        }

        fn restart_verified_parent(&mut self, parent_instance_id: &str) -> Result<(), Self::Error> {
            // Same bounded disable/enable as `restart_target`, applied to the
            // parent composite that `execute_recovery` already re-proved live
            // and identity-matched. Never reachable for a hub or controller:
            // the ladder only passes a `USB\VID_...` composite here.
            restart_target(parent_instance_id)
        }

        fn post_operation_poll_attempts(&self) -> usize {
            8
        }

        fn wait_for_post_operation_poll(&mut self) {
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    // These Config Manager functions are intentionally the only real PnP
    // writes in this module. Keeping them behind `UsbPnpBackend` makes it
    // impossible for the helper to call a broader operation than the trait.
    fn inspect_device(instance_id: &str, allow_phantom: bool) -> Result<UsbPnpDevice, String> {
        let devinst = locate(instance_id, allow_phantom)?;
        let actual_instance_id = device_id(devinst)?;
        let parent_instance_id = parent_id(devinst)?;
        let device_class = device_class(devinst)?;
        let health = device_health(devinst);
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
        })
    }

    fn reenumerate_parent(parent_instance_id: &str) -> Result<(), String> {
        let parent = locate(parent_instance_id, false)?;
        // SAFETY: `parent` was obtained from Config Manager for the exact
        // verified live parent. Flags are zero, requesting no broad scan.
        let result = unsafe { CM_Reenumerate_DevNode(parent, 0) };
        (result == CR_SUCCESS)
            .then_some(())
            .ok_or_else(|| format!("CM_Reenumerate_DevNode failed ({result})"))
    }

    fn restart_target(instance_id: &str) -> Result<(), String> {
        let target = locate(instance_id, false)?;
        // SAFETY: `target` was revalidated as the exact present problematic
        // child. The helper never passes a parent/hub/controller to this call.
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
            return Ok(fbuild_core::usb::UNCLASSED_DEVICE_CLASS.to_string());
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

    fn from_utf16(buffer: &[u16]) -> String {
        let length = buffer
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(buffer.len());
        String::from_utf16_lossy(&buffer[..length])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[derive(Default)]
    struct FakePnp {
        observations: VecDeque<Result<UsbPnpDevice, String>>,
        calls: Vec<String>,
    }

    impl FakePnp {
        fn with_observations(observations: Vec<UsbPnpDevice>) -> Self {
            Self {
                observations: observations.into_iter().map(Ok).collect(),
                calls: Vec::new(),
            }
        }
    }

    impl UsbPnpBackend for FakePnp {
        type Error = String;

        fn inspect(
            &mut self,
            instance_id: &str,
            allow_phantom: bool,
        ) -> Result<UsbPnpDevice, Self::Error> {
            self.calls
                .push(format!("inspect:{instance_id}:{allow_phantom}"));
            self.observations
                .pop_front()
                .unwrap_or_else(|| Err("unexpected inspect".to_string()))
        }

        fn reenumerate_parent(&mut self, parent_instance_id: &str) -> Result<(), Self::Error> {
            self.calls.push(format!("reenumerate:{parent_instance_id}"));
            Ok(())
        }

        fn restart_target(&mut self, instance_id: &str) -> Result<(), Self::Error> {
            self.calls.push(format!("restart:{instance_id}"));
            Ok(())
        }

        fn restart_verified_parent(&mut self, parent_instance_id: &str) -> Result<(), Self::Error> {
            self.calls
                .push(format!("restart-parent:{parent_instance_id}"));
            Ok(())
        }
    }

    fn request() -> UsbRecoveryRequest {
        UsbRecoveryRequest {
            operation_id: "deploy-1".to_string(),
            instance_id: "USB\\VID_2E8A&PID_000A\\serial".to_string(),
            expected_class: "Ports".to_string(),
            parent_instance_id: Some("USB\\ROOT_HUB30\\parent".to_string()),
            expected_vid: 0x2e8a,
            expected_pid: 0x000a,
            expected_serial: Some("serial".to_string()),
            problem_code: Some(43),
            flash_completed: true,
        }
    }

    fn device(health: UsbRecoveryHealth) -> UsbPnpDevice {
        UsbPnpDevice {
            instance_id: request().instance_id,
            parent_instance_id: request().parent_instance_id,
            device_class: request().expected_class,
            vid: 0x2e8a,
            pid: 0x000a,
            serial: Some("serial".to_string()),
            health,
        }
    }

    #[test]
    fn phantom_reenumerates_only_its_verified_live_parent() {
        let parent = UsbPnpDevice {
            instance_id: request().parent_instance_id.unwrap(),
            parent_instance_id: None,
            device_class: "USB".to_string(),
            vid: 0x2e8a,
            pid: 0x000a,
            serial: Some("serial".to_string()),
            health: UsbRecoveryHealth::HealthyPresent,
        };
        let mut backend = FakePnp::with_observations(vec![
            device(UsbRecoveryHealth::Phantom {
                problem_code: Some(43),
            }),
            parent,
            device(UsbRecoveryHealth::HealthyPresent),
        ]);

        let result = execute_recovery(&request(), "nonce".to_string(), &mut backend);

        assert!(result.success);
        assert_eq!(
            result.operation,
            Some(UsbRecoveryOperation::ReenumerateParent)
        );
        assert_eq!(
            backend.calls,
            vec![
                "inspect:USB\\VID_2E8A&PID_000A\\serial:true",
                "inspect:USB\\ROOT_HUB30\\parent:false",
                "reenumerate:USB\\ROOT_HUB30\\parent",
                "inspect:USB\\VID_2E8A&PID_000A\\serial:true",
            ]
        );
    }

    #[test]
    fn present_problem_restarts_only_the_exact_child() {
        let mut backend = FakePnp::with_observations(vec![
            device(UsbRecoveryHealth::PresentProblem { problem_code: 43 }),
            device(UsbRecoveryHealth::HealthyPresent),
        ]);

        let result = execute_recovery(&request(), "nonce".to_string(), &mut backend);

        assert!(result.success);
        assert_eq!(result.operation, Some(UsbRecoveryOperation::RestartTarget));
        assert!(
            backend
                .calls
                .iter()
                .any(|call| call == "restart:USB\\VID_2E8A&PID_000A\\serial")
        );
        assert!(
            !backend
                .calls
                .iter()
                .any(|call| call.starts_with("reenumerate:"))
        );
    }

    #[test]
    fn identity_mismatch_rejects_before_any_pnp_operation() {
        let mut mismatched = device(UsbRecoveryHealth::PresentProblem { problem_code: 43 });
        mismatched.pid = 0x000b;
        let mut backend = FakePnp::with_observations(vec![mismatched]);

        let result = execute_recovery(&request(), "nonce".to_string(), &mut backend);

        assert!(!result.success);
        assert_eq!(result.error_code.as_deref(), Some("vid-pid-mismatch"));
        assert_eq!(backend.calls.len(), 1);
    }

    #[test]
    fn class_serial_and_parent_mismatches_each_fail_closed() {
        type Mutation = fn(&mut UsbPnpDevice);
        let cases: [(&str, Mutation); 4] = [
            ("device-class-mismatch", |device: &mut UsbPnpDevice| {
                device.device_class = "USB".to_string();
            }),
            ("serial-mismatch", |device: &mut UsbPnpDevice| {
                device.serial = Some("different".to_string());
            }),
            ("parent-mismatch", |device: &mut UsbPnpDevice| {
                device.parent_instance_id = Some("USB\\ROOT_HUB30\\different".to_string());
            }),
            ("problem-code-mismatch", |device: &mut UsbPnpDevice| {
                device.health = UsbRecoveryHealth::PresentProblem { problem_code: 31 };
            }),
        ];
        for (expected_error, mutate) in cases {
            let mut target = device(UsbRecoveryHealth::PresentProblem { problem_code: 43 });
            mutate(&mut target);
            let mut backend = FakePnp::with_observations(vec![target]);
            let result = execute_recovery(&request(), "nonce".to_string(), &mut backend);
            assert!(!result.success, "{expected_error}");
            assert_eq!(result.error_code.as_deref(), Some(expected_error));
            assert_eq!(backend.calls.len(), 1, "{expected_error}");
        }
    }

    #[test]
    fn unhealthy_result_after_success_remains_advisory_not_a_port() {
        let mut backend = FakePnp::with_observations(vec![
            device(UsbRecoveryHealth::PresentProblem { problem_code: 43 }),
            device(UsbRecoveryHealth::PresentProblem { problem_code: 43 }),
        ]);

        let result = execute_recovery(&request(), "nonce".to_string(), &mut backend);

        assert!(result.success);
        assert!(matches!(
            result.after,
            UsbRecoveryHealth::PresentProblem { .. }
        ));
        assert!(result.validated_instance_id.is_some());
    }

    const BOOTSEL_INTERFACE: &str = "USB\\VID_2E8A&PID_0003&MI_01\\8&22CF742D&0&0001";
    const BOOTSEL_COMPOSITE: &str = "USB\\VID_2E8A&PID_0003\\E0C9125B0D9B";

    fn interface_request() -> UsbRecoveryRequest {
        UsbRecoveryRequest {
            operation_id: "deploy-2".to_string(),
            instance_id: BOOTSEL_INTERFACE.to_string(),
            expected_class: fbuild_core::usb::UNCLASSED_DEVICE_CLASS.to_string(),
            parent_instance_id: Some(BOOTSEL_COMPOSITE.to_string()),
            expected_vid: 0x2e8a,
            expected_pid: 0x0003,
            expected_serial: Some("E0C9125B0D9B".to_string()),
            problem_code: Some(28),
            flash_completed: false,
        }
    }

    fn interface_target(health: UsbRecoveryHealth) -> UsbPnpDevice {
        UsbPnpDevice {
            instance_id: BOOTSEL_INTERFACE.to_string(),
            parent_instance_id: Some(BOOTSEL_COMPOSITE.to_string()),
            device_class: fbuild_core::usb::UNCLASSED_DEVICE_CLASS.to_string(),
            vid: 0x2e8a,
            pid: 0x0003,
            serial: Some("E0C9125B0D9B".to_string()),
            health,
        }
    }

    fn composite_parent(health: UsbRecoveryHealth) -> UsbPnpDevice {
        UsbPnpDevice {
            instance_id: BOOTSEL_COMPOSITE.to_string(),
            parent_instance_id: Some("USB\\ROOT_HUB30\\5&23f8e3f5&0&0".to_string()),
            device_class: "USB".to_string(),
            vid: 0x2e8a,
            pid: 0x0003,
            serial: Some("E0C9125B0D9B".to_string()),
            health,
        }
    }

    #[test]
    fn problem_interface_restarts_only_its_verified_parent_composite() {
        let mut backend = FakePnp::with_observations(vec![
            interface_target(UsbRecoveryHealth::PresentProblem { problem_code: 28 }),
            composite_parent(UsbRecoveryHealth::HealthyPresent),
            interface_target(UsbRecoveryHealth::PresentProblem { problem_code: 28 }),
        ]);

        let result = execute_recovery(&interface_request(), "nonce".to_string(), &mut backend);

        assert!(result.success, "{:?}", result.error_code);
        assert_eq!(
            result.operation,
            Some(UsbRecoveryOperation::RestartVerifiedParent)
        );
        assert!(
            backend
                .calls
                .iter()
                .any(|call| call == &format!("restart-parent:{BOOTSEL_COMPOSITE}"))
        );
        assert!(
            !backend
                .calls
                .iter()
                .any(|call| call.starts_with("restart:USB"))
        );
    }

    #[test]
    fn problem_interface_with_unhealthy_parent_fails_closed() {
        let mut backend = FakePnp::with_observations(vec![
            interface_target(UsbRecoveryHealth::PresentProblem { problem_code: 28 }),
            composite_parent(UsbRecoveryHealth::PresentProblem { problem_code: 31 }),
        ]);

        let result = execute_recovery(&interface_request(), "nonce".to_string(), &mut backend);

        assert!(!result.success);
        assert_eq!(result.error_code.as_deref(), Some("parent-not-live"));
        assert!(
            !backend
                .calls
                .iter()
                .any(|call| call.starts_with("restart-parent:"))
        );
    }

    #[test]
    fn problem_interface_with_mismatched_parent_identity_fails_closed() {
        let mut wrong_identity = composite_parent(UsbRecoveryHealth::HealthyPresent);
        wrong_identity.pid = 0x000a;
        let mut backend = FakePnp::with_observations(vec![
            interface_target(UsbRecoveryHealth::PresentProblem { problem_code: 28 }),
            wrong_identity,
        ]);

        let result = execute_recovery(&interface_request(), "nonce".to_string(), &mut backend);

        assert!(!result.success);
        assert_eq!(result.error_code.as_deref(), Some("parent-not-live"));
    }

    #[test]
    fn problem_interface_without_parent_fact_fails_closed() {
        let mut request = interface_request();
        request.parent_instance_id = None;
        let mut target = interface_target(UsbRecoveryHealth::PresentProblem { problem_code: 28 });
        target.parent_instance_id = None;
        let mut backend = FakePnp::with_observations(vec![target]);

        let result = execute_recovery(&request, "nonce".to_string(), &mut backend);

        assert!(!result.success);
        assert_eq!(
            result.error_code.as_deref(),
            Some("missing-verified-parent")
        );
    }

    #[test]
    fn unclassed_sentinel_is_an_exact_class_match_not_a_wildcard() {
        let mut request = interface_request();
        request.expected_class = "Ports".to_string();
        let mut backend =
            FakePnp::with_observations(vec![interface_target(UsbRecoveryHealth::PresentProblem {
                problem_code: 28,
            })]);

        let result = execute_recovery(&request, "nonce".to_string(), &mut backend);

        assert!(!result.success);
        assert_eq!(result.error_code.as_deref(), Some("device-class-mismatch"));
    }
}
