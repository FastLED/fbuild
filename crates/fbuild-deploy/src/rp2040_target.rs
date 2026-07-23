//! Runtime CDC target selection for RP-series deployment.

use std::collections::BTreeSet;

use fbuild_core::{FbuildError, Result};

use super::PicoCdcPort;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RequestedRuntimeTarget {
    pub(super) port: String,
    pub(super) serial_number: Option<String>,
}

pub(super) fn serial_selector(selector: &str) -> Option<&str> {
    selector
        .get(..4)
        .filter(|prefix| prefix.eq_ignore_ascii_case("SER="))
        .map(|_| &selector[4..])
        .filter(|serial| !serial.is_empty())
}

/// Identity/health line for refusing or reporting a known-unhealthy record
/// (FastLED/fbuild#1147: diagnostic visibility is not deploy eligibility).
pub(super) fn describe_unhealthy(port: &PicoCdcPort) -> String {
    let problem = port
        .health
        .problem_code()
        .map(|code| format!("; problem code {code}"))
        .unwrap_or_default();
    let instance = port
        .instance_id
        .as_deref()
        .map(|value| format!("; instance {value}"))
        .unwrap_or_default();
    format!(
        "{} (health {}{problem}{instance})",
        port.name,
        port.health.label()
    )
}

pub(super) fn resolve_requested_runtime_target(
    selector: &str,
    candidates: &[PicoCdcPort],
) -> Result<RequestedRuntimeTarget> {
    let matches: Vec<_> = if let Some(serial) = serial_selector(selector) {
        candidates
            .iter()
            .filter(|candidate| candidate.serial_number.as_deref() == Some(serial))
            .collect()
    } else {
        candidates
            .iter()
            .filter(|candidate| candidate.name == selector)
            .collect()
    };
    match matches.as_slice() {
        // An explicit selector never overrides known-unhealthy Windows PnP
        // state: a phantom/problem devnode is a historical record, not a live
        // endpoint, and opening or 1200-baud-touching it is guaranteed stale
        // (FastLED/fbuild#1147).
        [only] if only.health.is_known_unhealthy() => Err(FbuildError::DeployFailed(format!(
            "RP2040 runtime selector {selector:?} matches {}, which Windows reports as not usable; fbuild will not open or reset a stale devnode record — reconnect the board (or enter BOOTSEL) and retry",
            describe_unhealthy(only)
        ))),
        [only] => Ok(RequestedRuntimeTarget {
            port: only.name.clone(),
            serial_number: only.serial_number.clone(),
        }),
        [] => Err(FbuildError::DeployFailed(format!(
            "RP2040 runtime selector {selector:?} did not match a catalogue-identified CDC port"
        ))),
        many => Err(FbuildError::DeployFailed(format!(
            "RP2040 runtime selector {selector:?} is ambiguous across: {}",
            many.iter()
                .map(|port| port.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// Select the post-flash runtime CDC endpoint from one fresh catalogue scan.
///
/// Known-unhealthy (phantom / present-problem) records stay visible in the
/// caller's diagnostics but are never eligible here (FastLED/fbuild#1147): a
/// serial or name match against a stale Windows devnode proves history, not a
/// live endpoint. Returning `Ok(None)` keeps the caller polling until its
/// bounded deadline, when the unhealthy records surface in the timeout
/// diagnostics.
pub(super) fn select_cdc_candidate(
    previous_port: Option<&str>,
    requested_serial: Option<&str>,
    before: &BTreeSet<String>,
    ports: &[PicoCdcPort],
) -> Result<Option<PicoCdcPort>> {
    let eligible: Vec<&PicoCdcPort> = ports
        .iter()
        .filter(|port| !port.health.is_known_unhealthy())
        .collect();
    if let Some(serial) = requested_serial {
        let matching: Vec<_> = eligible
            .iter()
            .filter(|port| port.serial_number.as_deref() == Some(serial))
            .collect();
        return match matching.as_slice() {
            [] => Ok(None),
            [only] => Ok(Some((**only).clone())),
            many => Err(FbuildError::DeployFailed(format!(
                "multiple Raspberry Pi CDC interfaces have USB serial {serial}: {}",
                many.iter()
                    .map(|port| port.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        };
    }
    if let Some(old) = previous_port {
        if let Some(port) = eligible.iter().find(|port| port.name == old) {
            return Ok(Some((*port).clone()));
        }
    }
    let new_ports: Vec<_> = eligible
        .iter()
        .filter(|port| !before.contains(&port.name))
        .collect();
    match new_ports.as_slice() {
        [] => Ok(None),
        [only] => Ok(Some((**only).clone())),
        many => Err(FbuildError::DeployFailed(format!(
            "multiple new Raspberry Pi CDC ports appeared after deploy: {}; pass -p/--port to select one",
            many.iter()
                .map(|port| port.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fbuild_serial::ports::PortHealth;

    fn cdc(name: &str, serial_number: Option<&str>) -> PicoCdcPort {
        cdc_with_health(name, serial_number, PortHealth::Unknown)
    }

    fn cdc_with_health(name: &str, serial_number: Option<&str>, health: PortHealth) -> PicoCdcPort {
        PicoCdcPort {
            name: name.to_string(),
            serial_number: serial_number.map(str::to_string),
            health,
            instance_id: Some(format!("USB\\VID_2E8A&PID_000A\\{name}")),
            parent_instance_id: None,
        }
    }

    fn selected_name(candidate: Option<PicoCdcPort>) -> Option<String> {
        candidate.map(|port| port.name)
    }

    #[test]
    fn changed_cdc_port_is_returned_instead_of_stale_port() {
        let before = BTreeSet::from(["COM7".to_string()]);
        let after = vec![cdc("COM12", Some("PICO-1"))];
        assert_eq!(
            selected_name(select_cdc_candidate(Some("COM7"), None, &before, &after).unwrap()),
            Some("COM12".to_string())
        );
    }

    #[test]
    fn usb_serial_selector_survives_port_renumbering() {
        let before = BTreeSet::from(["COM7".to_string()]);
        let after = vec![cdc("COM12", Some("PICO-1")), cdc("COM13", Some("PICO-2"))];
        assert_eq!(
            selected_name(
                select_cdc_candidate(Some("COM7"), Some("PICO-1"), &before, &after).unwrap()
            ),
            Some("COM12".to_string())
        );
        assert_eq!(
            resolve_requested_runtime_target("SER=PICO-2", &after).unwrap(),
            RequestedRuntimeTarget {
                port: "COM13".to_string(),
                serial_number: Some("PICO-2".to_string()),
            }
        );
    }

    #[test]
    fn multiple_new_cdc_ports_are_rejected() {
        let error = select_cdc_candidate(
            None,
            None,
            &BTreeSet::new(),
            &[cdc("COM12", None), cdc("COM13", None)],
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("multiple new Raspberry Pi CDC ports")
        );
    }

    #[test]
    fn phantom_serial_match_is_never_selected() {
        let phantom = cdc_with_health(
            "COM12",
            Some("5303284720C4641C"),
            PortHealth::Phantom {
                problem_code: None,
                status: None,
            },
        );
        let result = select_cdc_candidate(
            Some("COM12"),
            Some("5303284720C4641C"),
            &BTreeSet::new(),
            &[phantom],
        )
        .unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn present_problem_previous_port_is_never_reselected() {
        let broken = cdc_with_health(
            "COM12",
            None,
            PortHealth::PresentProblem {
                problem_code: 31,
                status: None,
            },
        );
        let before = BTreeSet::from(["COM12".to_string()]);
        let result = select_cdc_candidate(Some("COM12"), None, &before, &[broken]).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn healthy_record_wins_over_phantom_history_with_same_serial() {
        let ports = vec![
            cdc_with_health(
                "COM12",
                Some("5303284720C4641C"),
                PortHealth::Phantom {
                    problem_code: None,
                    status: None,
                },
            ),
            cdc_with_health(
                "COM27",
                Some("5303284720C4641C"),
                PortHealth::HealthyPresent,
            ),
        ];
        let selected = select_cdc_candidate(
            Some("COM12"),
            Some("5303284720C4641C"),
            &BTreeSet::from(["COM12".to_string()]),
            &ports,
        )
        .unwrap()
        .expect("the healthy renumbered endpoint must be selected");
        assert_eq!(selected.name, "COM27");
        assert_eq!(selected.health, PortHealth::HealthyPresent);
    }

    #[test]
    fn unhealthy_new_port_does_not_create_ambiguity() {
        let ports = vec![
            cdc_with_health(
                "COM12",
                None,
                PortHealth::Phantom {
                    problem_code: None,
                    status: None,
                },
            ),
            cdc_with_health("COM27", None, PortHealth::HealthyPresent),
        ];
        assert_eq!(
            selected_name(select_cdc_candidate(None, None, &BTreeSet::new(), &ports).unwrap()),
            Some("COM27".to_string())
        );
    }

    #[test]
    fn explicit_selector_matching_a_phantom_fails_with_health_details() {
        let ports = vec![cdc_with_health(
            "COM12",
            Some("5303284720C4641C"),
            PortHealth::Phantom {
                problem_code: Some(45),
                status: None,
            },
        )];
        for selector in ["COM12", "SER=5303284720C4641C"] {
            let message = resolve_requested_runtime_target(selector, &ports)
                .unwrap_err()
                .to_string();
            assert!(
                message.contains("health phantom"),
                "missing health: {message}"
            );
            assert!(
                message.contains("problem code 45"),
                "missing code: {message}"
            );
            assert!(
                message.contains("USB\\VID_2E8A&PID_000A\\COM12"),
                "missing instance: {message}"
            );
        }
    }

    #[test]
    fn unknown_health_remains_eligible_for_explicit_selection() {
        let ports = vec![cdc("COM5", Some("PICO-9"))];
        assert!(resolve_requested_runtime_target("COM5", &ports).is_ok());
        assert!(resolve_requested_runtime_target("SER=PICO-9", &ports).is_ok());
    }
}
