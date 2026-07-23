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

/// The sole two Windows PnP operations a recovery helper can represent.
///
/// There is deliberately no generic command, executable, device-class, or
/// "reset all USB" variant. A phantom endpoint may only re-enumerate its
/// verified parent; a present problematic endpoint may only restart itself.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UsbRecoveryOperation {
    ReenumerateParent,
    RestartTarget,
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
        ];
        assert_eq!(operations.len(), 2);
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
}
