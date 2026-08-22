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
    normalize_physical_location,
};

/// A PnP devnode observed directly by the recovery backend. The record
/// itself is a host-neutral fact owned by the platform facade; the recovery
/// ladder only revalidates its fields.
pub use fbuild_core::platform::device::UsbPnpDevice;

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
    if let Some(expected_location) = request.expected_location_path.as_deref() {
        match (request.problem_code, &target.health) {
            (Some(expected_problem_code), UsbRecoveryHealth::PresentProblem { problem_code })
                if *problem_code == expected_problem_code => {}
            (Some(_), UsbRecoveryHealth::PresentProblem { .. }) => {
                return Err("problem-code-mismatch");
            }
            _ => return Err("location-target-not-present-problem"),
        }
        if !target
            .location_paths
            .iter()
            .any(|path| normalize_physical_location(path).as_deref() == Some(expected_location))
        {
            return Err("location-path-mismatch");
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
    if !fbuild_core::platform::host::is_windows() {
        return UsbRecoveryResult {
            operation_id: request.operation_id.clone(),
            nonce,
            validated_instance_id: None,
            operation: None,
            before: UsbRecoveryHealth::Unknown,
            after: UsbRecoveryHealth::Unknown,
            success: false,
            error_code: Some("windows-recovery-unavailable".to_string()),
        };
    }
    let mut backend = PlatformPnpBackend;
    execute_recovery(request, nonce, &mut backend)
}

/// Real backend over the neutral device facade. The Config Manager writes it
/// reaches are confined to the exact devnodes `execute_recovery` revalidated;
/// on hosts without a Windows PnP surface every primitive fails closed, and
/// [`recover_windows_usb_device`] never even constructs this backend there.
struct PlatformPnpBackend;

impl UsbPnpBackend for PlatformPnpBackend {
    type Error = String;

    fn inspect(
        &mut self,
        instance_id: &str,
        allow_phantom: bool,
    ) -> Result<UsbPnpDevice, Self::Error> {
        fbuild_core::platform::device::inspect_usb_pnp_device(instance_id, allow_phantom)
    }

    fn reenumerate_parent(&mut self, parent_instance_id: &str) -> Result<(), Self::Error> {
        fbuild_core::platform::device::reenumerate_usb_parent(parent_instance_id)
    }

    fn restart_target(&mut self, instance_id: &str) -> Result<(), Self::Error> {
        fbuild_core::platform::device::restart_usb_device(instance_id)
    }

    fn restart_verified_parent(&mut self, parent_instance_id: &str) -> Result<(), Self::Error> {
        // Same bounded disable/enable as `restart_target`, applied to the
        // parent composite that `execute_recovery` already re-proved live
        // and identity-matched. Never reachable for a hub or controller:
        // the ladder only passes a `USB\VID_...` composite here.
        fbuild_core::platform::device::restart_usb_device(parent_instance_id)
    }

    fn post_operation_poll_attempts(&self) -> usize {
        fbuild_core::platform::device::usb_pnp_post_operation_poll_attempts()
    }

    fn wait_for_post_operation_poll(&mut self) {
        std::thread::sleep(fbuild_core::platform::device::usb_pnp_post_operation_poll_interval());
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
            descriptor_failure_at_location: false,
            expected_location_path: None,
            problem_code: Some(43),
            flash_completed: true,
        }
    }

    fn descriptor_failure_request(location: Option<&str>) -> UsbRecoveryRequest {
        let mut request = request();
        request.expected_vid = 0;
        request.expected_pid = 2;
        request.expected_serial = None;
        request.descriptor_failure_at_location = true;
        request.expected_location_path = location.map(str::to_string);
        request
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
            location_paths: Vec::new(),
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
            location_paths: Vec::new(),
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
    fn descriptor_failure_recovery_revalidates_normalized_location_path() {
        let request = descriptor_failure_request(Some("PCIROOT(0)#USBROOT(0)#USB(10)#USB(4)"));
        let mut target = device(UsbRecoveryHealth::PresentProblem { problem_code: 43 });
        target.vid = 0;
        target.pid = 2;
        target.serial = None;
        target.location_paths = vec!["pciroot(0)#usbroot(0)#usb(10)#usb(4)#usbmi(0)".to_string()];
        assert_eq!(validate_target_identity(&request, &target), Ok(()));

        target.location_paths = vec!["PCIROOT(0)#USBROOT(0)#USB(14)".to_string()];
        assert_eq!(
            validate_target_identity(&request, &target),
            Err("location-path-mismatch")
        );
    }

    #[test]
    fn descriptor_failure_that_became_phantom_never_reenumerates_parent() {
        let mut request = descriptor_failure_request(Some("PCIROOT(0)#USBROOT(0)#USB(10)#USB(4)"));
        request.instance_id = "USB\\VID_0000&PID_0002\\descriptor-failed".to_string();
        request.expected_class = "USB".to_string();

        let mut target = device(UsbRecoveryHealth::Phantom {
            problem_code: Some(43),
        });
        target.instance_id = request.instance_id.clone();
        target.device_class = request.expected_class.clone();
        target.vid = 0;
        target.pid = 2;
        target.serial = None;
        target.location_paths = vec!["PCIROOT(0)#USBROOT(0)#USB(10)#USB(4)".to_string()];
        let mut backend = FakePnp::with_observations(vec![target]);

        let result = execute_recovery(&request, "nonce".to_string(), &mut backend);

        assert!(!result.success);
        assert_eq!(
            result.error_code.as_deref(),
            Some("location-target-not-present-problem")
        );
        assert_eq!(
            backend.calls,
            vec!["inspect:USB\\VID_0000&PID_0002\\descriptor-failed:true"]
        );
    }

    #[test]
    fn descriptor_failure_without_location_is_rejected_before_inspection() {
        let mut request = descriptor_failure_request(None);
        request.instance_id = "USB\\VID_0000&PID_0002\\descriptor-failed".to_string();
        request.expected_class = "USB".to_string();
        request.problem_code = Some(43);
        let mut backend =
            FakePnp::with_observations(vec![device(UsbRecoveryHealth::PresentProblem {
                problem_code: 43,
            })]);

        let result = execute_recovery(&request, "nonce".to_string(), &mut backend);

        assert!(!result.success);
        assert_eq!(
            result.error_code.as_deref(),
            Some("invalid-request-identity")
        );
        assert!(backend.calls.is_empty());
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
            descriptor_failure_at_location: false,
            expected_location_path: None,
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
            location_paths: Vec::new(),
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
            location_paths: Vec::new(),
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
