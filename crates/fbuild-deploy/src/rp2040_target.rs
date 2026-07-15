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

pub(super) fn select_cdc_candidate(
    previous_port: Option<&str>,
    requested_serial: Option<&str>,
    before: &BTreeSet<String>,
    ports: &[PicoCdcPort],
) -> Result<Option<String>> {
    if let Some(serial) = requested_serial {
        let matching: Vec<_> = ports
            .iter()
            .filter(|port| port.serial_number.as_deref() == Some(serial))
            .collect();
        return match matching.as_slice() {
            [] => Ok(None),
            [only] => Ok(Some(only.name.clone())),
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
        if ports.iter().any(|port| port.name == old) {
            return Ok(Some(old.to_string()));
        }
    }
    let new_names: Vec<_> = ports
        .iter()
        .filter(|port| !before.contains(&port.name))
        .map(|port| port.name.clone())
        .collect();
    match new_names.as_slice() {
        [] => Ok(None),
        [only] => Ok(Some(only.clone())),
        many => Err(FbuildError::DeployFailed(format!(
            "multiple new Raspberry Pi CDC ports appeared after deploy: {}; pass -p/--port to select one",
            many.join(", ")
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cdc(name: &str, serial_number: Option<&str>) -> PicoCdcPort {
        PicoCdcPort {
            name: name.to_string(),
            serial_number: serial_number.map(str::to_string),
        }
    }

    #[test]
    fn changed_cdc_port_is_returned_instead_of_stale_port() {
        let before = BTreeSet::from(["COM7".to_string()]);
        let after = vec![cdc("COM12", Some("PICO-1"))];
        assert_eq!(
            select_cdc_candidate(Some("COM7"), None, &before, &after).unwrap(),
            Some("COM12".to_string())
        );
    }

    #[test]
    fn usb_serial_selector_survives_port_renumbering() {
        let before = BTreeSet::from(["COM7".to_string()]);
        let after = vec![cdc("COM12", Some("PICO-1")), cdc("COM13", Some("PICO-2"))];
        assert_eq!(
            select_cdc_candidate(Some("COM7"), Some("PICO-1"), &before, &after).unwrap(),
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
        assert!(error
            .to_string()
            .contains("multiple new Raspberry Pi CDC ports"));
    }
}
