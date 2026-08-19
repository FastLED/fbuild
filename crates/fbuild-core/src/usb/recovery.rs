//! Narrow, typed contract for one-shot USB PnP recovery.
//!
//! This module deliberately contains no host API calls. The normal daemon
//! creates a [`UsbRecoveryRequest`] only for an exact unhealthy endpoint; the
//! elevated CLI helper revalidates that identity and then asks `fbuild-serial`
//! to perform one of the two allowlisted operations. Keeping this contract in
//! `fbuild-core` lets the daemon and CLI communicate without parsing a human
//! diagnostic or making the daemon privileged. FastLED/fbuild#1148.

use serde::{Deserialize, Serialize};

/// The caller's explicit permission to request the one-shot elevated helper.
///
/// `Default` is intentionally non-elevating: it asks the user to rerun with
/// `--admin`. `DenyAdmin` is the `--no-admin` escape hatch. CI and
/// non-interactive checks remain an additional guard in the CLI and cannot be
/// bypassed by this enum.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UsbRecoveryPolicy {
    #[default]
    Default,
    AllowAdmin,
    DenyAdmin,
}

/// The sole Windows PnP operations a recovery helper can represent.
///
/// There is deliberately no generic command, executable, device-class, or
/// "reset all USB" variant. A phantom endpoint may only re-enumerate its
/// verified parent; a present problematic endpoint may only restart itself;
/// and a present problematic USB *interface* devnode (a `&MI_xx` child of a
/// composite device, e.g. the RP2040 BOOTSEL PICOBOOT function) may only
/// restart its verified healthy parent composite, because restarting the
/// interface alone cannot re-initialize the sibling interfaces or remount
/// the synthetic BOOTSEL volume (FastLED/fbuild#1152).
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UsbRecoveryOperation {
    ReenumerateParent,
    RestartTarget,
    RestartVerifiedParent,
}

/// Sentinel for a devnode with no Windows device class (typical for a
/// driverless interface such as a BOOTSEL PICOBOOT function reporting
/// `CM_PROB_FAILED_INSTALL`). Used on both sides of the identity
/// revalidation so an absent class is an exact-match fact, not a wildcard.
pub const UNCLASSED_DEVICE_CLASS: &str = "(none)";

/// Windows' USB descriptor-request-failure identity. This is an operating-
/// system protocol sentinel, not a board VID/PID record; board identities
/// remain sourced exclusively from the verified FastLED/boards catalogue.
pub const WINDOWS_DESCRIPTOR_FAILURE_VID: u16 = 0;
pub const WINDOWS_DESCRIPTOR_FAILURE_PID: u16 = 2;

pub fn is_windows_descriptor_failure_identity(vid: u16, pid: u16) -> bool {
    vid == WINDOWS_DESCRIPTOR_FAILURE_VID && pid == WINDOWS_DESCRIPTOR_FAILURE_PID
}

/// Host health observed before or after a recovery operation.
///
/// This is intentionally independent of `fbuild_serial::PortHealth` so the
/// core contract does not introduce a dependency cycle. A helper result is
/// advisory only: the normal process must later perform a fresh serial
/// enumeration and openability probe before it returns a usable port.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UsbRecoveryHealth {
    HealthyPresent,
    PresentProblem { problem_code: u32 },
    Phantom { problem_code: Option<u32> },
    Unknown,
}

/// Identity facts the elevated helper must re-query before it can act.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UsbRecoveryRequest {
    /// Opaque daemon operation ID used only to correlate the one-shot result.
    pub operation_id: String,
    /// Canonical Windows PnP instance ID for the unhealthy endpoint.
    pub instance_id: String,
    /// Windows device class expected for that exact endpoint (for example,
    /// `Ports`). The helper rejects a matching-looking USB instance that has
    /// moved to a different class.
    pub expected_class: String,
    /// Immediate parent instance ID as observed by the normal process.
    pub parent_instance_id: Option<String>,
    /// USB identity expected from the same endpoint, never a runtime default.
    pub expected_vid: u16,
    pub expected_pid: u16,
    /// Required when the board profile supplied a serial number.
    pub expected_serial: Option<String>,
    /// True only when Windows reported a descriptor-failed USB node that was
    /// correlated to one historical board by an exact physical location.
    /// The helper still revalidates the node's observed VID/PID and location.
    #[serde(default)]
    pub descriptor_failure_at_location: bool,
    /// Normalized physical USB location that must still match when recovering
    /// a descriptor-failed node whose current VID/PID cannot identify the
    /// board. `None` for ordinary identity-bound recovery requests.
    #[serde(default)]
    pub expected_location_path: Option<String>,
    /// Problem code observed by the normal process, if Windows supplied one.
    pub problem_code: Option<u32>,
    /// Distinguishes preflight recovery from post-flash recovery-only flow.
    pub flash_completed: bool,
}

impl UsbRecoveryRequest {
    /// Reject fields that cannot be safe canonical PnP identity input.
    ///
    /// The helper performs a second, authoritative host re-query. This check
    /// nevertheless prevents a malformed rendezvous file from reaching any
    /// Windows operation or being reported as a recoverable target.
    pub fn has_canonical_identity(&self) -> bool {
        fn canonical_pnp_id(value: &str) -> bool {
            !value.is_empty()
                && value.len() <= 512
                && !value.chars().any(|character| {
                    character.is_control() || matches!(character, '"' | '\'' | '\n' | '\r' | '\t')
                })
        }

        let location_bound_shape_is_safe = if self.descriptor_failure_at_location {
            is_windows_descriptor_failure_identity(self.expected_vid, self.expected_pid)
                && self.expected_location_path.is_some()
                && self.expected_serial.is_none()
                && self.problem_code == Some(43)
        } else {
            self.expected_vid != 0 && self.expected_location_path.is_none()
        };

        canonical_pnp_id(&self.operation_id)
            && canonical_pnp_id(&self.instance_id)
            && canonical_pnp_id(&self.expected_class)
            && self
                .parent_instance_id
                .as_deref()
                .map_or(true, canonical_pnp_id)
            && self.expected_serial.as_deref().map_or(true, |serial| {
                !serial.is_empty() && serial.len() <= 256 && !serial.chars().any(char::is_control)
            })
            && self.expected_location_path.as_deref().map_or(true, |path| {
                !path.is_empty()
                    && path.len() <= 1024
                    && !path.chars().any(|character| {
                        character.is_control()
                            || matches!(character, '"' | '\'' | '\n' | '\r' | '\t')
                    })
            })
            && location_bound_shape_is_safe
    }
}

/// Bounded, non-port-bearing response from the elevated helper.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UsbRecoveryResult {
    pub operation_id: String,
    pub nonce: String,
    pub validated_instance_id: Option<String>,
    pub operation: Option<UsbRecoveryOperation>,
    pub before: UsbRecoveryHealth,
    pub after: UsbRecoveryHealth,
    pub success: bool,
    /// Stable internal failure category, never a shell or operating-system
    /// command. The normal CLI renders the actionable user-facing guidance.
    pub error_code: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> UsbRecoveryRequest {
        UsbRecoveryRequest {
            operation_id: "deploy-123".to_string(),
            instance_id: "USB\\VID_2E8A&PID_000A\\5303284720C4641C".to_string(),
            expected_class: "Ports".to_string(),
            parent_instance_id: Some("USB\\ROOT_HUB30\\4&1".to_string()),
            expected_vid: 0x2e8a,
            expected_pid: 0x000a,
            expected_serial: Some("5303284720C4641C".to_string()),
            descriptor_failure_at_location: false,
            expected_location_path: None,
            problem_code: Some(43),
            flash_completed: true,
        }
    }

    #[test]
    fn recovery_policy_defaults_to_non_elevating() {
        assert_eq!(UsbRecoveryPolicy::default(), UsbRecoveryPolicy::Default);
    }

    #[test]
    fn recovery_operations_cannot_represent_a_broad_usb_reset() {
        let operations = [
            UsbRecoveryOperation::ReenumerateParent,
            UsbRecoveryOperation::RestartTarget,
            UsbRecoveryOperation::RestartVerifiedParent,
        ];
        assert_eq!(operations.len(), 3);
    }

    #[test]
    fn canonical_request_identity_rejects_control_and_quote_injection() {
        assert!(request().has_canonical_identity());

        let mut bad_instance = request();
        bad_instance.instance_id = "USB\\VID_2E8A\n--anything".to_string();
        assert!(!bad_instance.has_canonical_identity());

        let mut bad_parent = request();
        bad_parent.parent_instance_id = Some("USB\\ROOT_HUB\"".to_string());
        assert!(!bad_parent.has_canonical_identity());

        let mut bad_class = request();
        bad_class.expected_class = "Ports\nUSB".to_string();
        assert!(!bad_class.has_canonical_identity());
    }

    #[test]
    fn location_bound_request_requires_descriptor_failure_shape() {
        let mut location_bound = request();
        location_bound.instance_id = "USB\\VID_0000&PID_0002\\descriptor-failed".to_string();
        location_bound.expected_class = "USB".to_string();
        location_bound.expected_vid = 0;
        location_bound.expected_pid = 2;
        location_bound.expected_serial = None;
        location_bound.descriptor_failure_at_location = true;
        location_bound.expected_location_path = Some("PCIROOT(0)#USBROOT(0)#USB(4)".to_string());
        location_bound.problem_code = Some(43);
        assert!(location_bound.has_canonical_identity());

        let mut missing_code = location_bound.clone();
        missing_code.problem_code = None;
        assert!(!missing_code.has_canonical_identity());

        let mut wrong_identity = location_bound.clone();
        wrong_identity.expected_vid = 0x2e8a;
        assert!(!wrong_identity.has_canonical_identity());

        let mut wrong_descriptor_failure_pid = location_bound.clone();
        wrong_descriptor_failure_pid.expected_pid = 3;
        assert!(!wrong_descriptor_failure_pid.has_canonical_identity());

        let mut missing_descriptor_failure_fact = location_bound.clone();
        missing_descriptor_failure_fact.descriptor_failure_at_location = false;
        assert!(!missing_descriptor_failure_fact.has_canonical_identity());

        let mut unexpected_serial = location_bound;
        unexpected_serial.expected_serial = Some("not-authoritative".to_string());
        assert!(!unexpected_serial.has_canonical_identity());

        let mut missing_location = request();
        missing_location.expected_vid = 0;
        missing_location.expected_pid = 2;
        missing_location.expected_serial = None;
        missing_location.descriptor_failure_at_location = true;
        missing_location.problem_code = Some(43);
        assert!(!missing_location.has_canonical_identity());
    }
}
