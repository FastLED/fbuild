use crate::device_manager::DeviceState;
use fbuild_config::BoardConfig;
use fbuild_core::Platform;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DeployPortChoice {
    pub port: Option<String>,
    pub warning: Option<String>,
}

#[derive(Debug, Clone)]
struct PortCandidate {
    port: String,
    vid: Option<u16>,
    pid: Option<u16>,
    description: String,
}

pub(super) fn choose_deploy_port(
    requested: Option<String>,
    platform: Platform,
    board: Option<&BoardConfig>,
    devices: Vec<DeviceState>,
) -> DeployPortChoice {
    if requested.is_some() {
        return DeployPortChoice {
            port: requested,
            warning: None,
        };
    }

    // RP2040/RP2350 stock boards deploy through the ROM BOOTSEL volume and
    // may have no application CDC port at all before the UF2 copy. Never
    // fall back to an unrelated serial endpoint (for example Windows COM1);
    // the RP2040 deployer discovers the post-flash CDC port itself.
    if platform == Platform::RaspberryPi {
        return DeployPortChoice {
            port: None,
            warning: None,
        };
    }

    let expected_vids = expected_vids(platform, board);
    if expected_vids.is_empty() {
        return DeployPortChoice {
            port: None,
            warning: None,
        };
    }

    let mut candidates: Vec<_> = devices
        .into_iter()
        .filter(|d| d.is_connected)
        .map(|d| PortCandidate {
            port: d.port,
            vid: d.vid,
            pid: d.pid,
            description: d.description,
        })
        .collect();
    candidates.sort_by(|a, b| a.port.cmp(&b.port));

    let matches: Vec<_> = candidates
        .iter()
        .filter(|d| d.vid.is_some_and(|vid| expected_vids.contains(&vid)))
        .collect();

    if matches.len() == 1 {
        let selected = matches[0];
        log_connect("deploy", selected);
        DeployPortChoice {
            port: Some(selected.port.clone()),
            warning: None,
        }
    } else if !matches.is_empty() {
        let selected = matches[0];
        log_connect("deploy", selected);
        DeployPortChoice {
            port: Some(selected.port.clone()),
            warning: Some(format!(
                "multiple serial ports matched expected VID(s) {}; selected {} deterministically from {}; pass -p/--port to choose explicitly",
                format_vids(&expected_vids),
                selected.port,
                format_candidates(matches.iter().copied()),
            )),
        }
    } else if !candidates.is_empty() {
        let selected = &candidates[0];
        log_connect("deploy", selected);
        DeployPortChoice {
                port: Some(selected.port.clone()),
                warning: Some(format!(
                    "no serial port matched expected VID(s) {}; falling back to {} from {}; pass -p/--port to choose explicitly",
                    format_vids(&expected_vids),
                    selected.port,
                    format_candidates(candidates.iter()),
                )),
            }
    } else {
        DeployPortChoice {
            port: None,
            warning: Some(format!(
                "no serial ports found while looking for expected VID(s) {}",
                format_vids(&expected_vids)
            )),
        }
    }
}

pub(super) fn append_warning_to_stderr(stderr: &mut Option<String>, warning: Option<String>) {
    let Some(warning) = warning else {
        return;
    };
    let warning = format!("warning: {}", warning);
    match stderr {
        Some(existing) if !existing.is_empty() => {
            existing.push('\n');
            existing.push_str(&warning);
        }
        Some(existing) => existing.push_str(&warning),
        None => *stderr = Some(warning),
    }
}

fn expected_vids(platform: Platform, board: Option<&BoardConfig>) -> Vec<u16> {
    let mut vids = Vec::new();
    if let Some(vid) = board.and_then(|b| parse_u16_id(b.vid.as_deref())) {
        vids.push(vid);
    }

    let defaults: &[u16] = match platform {
        Platform::Teensy => &[0x16C0],
        Platform::Espressif32 => &[0x303A],
        Platform::AtmelAvr | Platform::AtmelMegaAvr => &[0x2341, 0x2A03, 0x1A86, 0x10C4, 0x0403],
        Platform::NxpLpc => &[0x1FC9, 0x0D28],
        Platform::RaspberryPi => &[0x2E8A],
        _ => &[],
    };

    for vid in defaults {
        if !vids.contains(vid) {
            vids.push(*vid);
        }
    }
    vids
}

fn parse_u16_id(value: Option<&str>) -> Option<u16> {
    let raw = value?.trim();
    let raw = raw
        .strip_prefix("0x")
        .or_else(|| raw.strip_prefix("0X"))
        .unwrap_or(raw);
    u16::from_str_radix(raw, 16)
        .or_else(|_| raw.parse::<u16>())
        .ok()
}

fn format_vids(vids: &[u16]) -> String {
    vids.iter()
        .map(|v| format!("0x{v:04X}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_candidates<'a>(candidates: impl Iterator<Item = &'a PortCandidate>) -> String {
    candidates
        .map(|d| {
            // For candidates we have a resolved VID:PID for, emit the
            // canonical `vendor product (VVVV:PPPP)` form via the shared
            // resolver — this is what the user sees in `fbuild device list`
            // and what we log on connect, so warnings stay consistent.
            let pretty = match (d.vid, d.pid) {
                (Some(vid), Some(pid)) => fbuild_core::usb::pretty(vid, pid),
                (Some(vid), None) => format!("{} ({vid:04X}:????)", d.description),
                _ => d.description.clone(),
            };
            format!("{} ({})", d.port, pretty)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Emit the canonical connect-time log line:
/// `"<op>: selected <port> — <vendor> <product> (VVVV:PPPP)"`. Falls back
/// to the raw `description` when no VID:PID is known. Called by
/// [`choose_deploy_port`] at the moment a device is bound to a deploy
/// operation; the same format is used by the scan log lines so the user
/// sees identical strings in `fbuild device list` and `fbuild deploy`.
fn log_connect(op: &str, candidate: &PortCandidate) {
    let pretty = match (candidate.vid, candidate.pid) {
        (Some(vid), Some(pid)) => fbuild_core::usb::pretty(vid, pid),
        (Some(vid), None) => format!("{} ({vid:04X}:????)", candidate.description),
        _ => candidate.description.clone(),
    };
    tracing::info!("{op}: selected {} — {}", candidate.port, pretty);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn device(port: &str, vid: Option<u16>, pid: Option<u16>) -> DeviceState {
        DeviceState {
            device_id: vid
                .map(|v| format!("{v:04x}:{:04x}", pid.unwrap_or(0)))
                .unwrap_or_else(|| port.to_string()),
            port: port.to_string(),
            description: "USB Serial Device".to_string(),
            vid,
            pid,
            vendor_name: None,
            product_name: None,
            is_cdc: None,
            serial_number: None,
            previous_port: None,
            exclusive_lease: None,
            monitor_leases: HashMap::new(),
            last_seen_at: 0.0,
            is_connected: true,
            trusted_firmware: None,
            last_disconnect_at: None,
        }
    }

    #[test]
    fn explicit_port_wins() {
        let choice = choose_deploy_port(
            Some("COM21".to_string()),
            Platform::Teensy,
            None,
            vec![device("COM22", Some(0x303A), Some(0x1001))],
        );
        assert_eq!(choice.port.as_deref(), Some("COM21"));
        assert!(choice.warning.is_none());
    }

    #[test]
    fn stock_raspberry_pi_deploy_does_not_select_unrelated_serial_port() {
        let choice = choose_deploy_port(
            None,
            Platform::RaspberryPi,
            None,
            vec![device("COM1", None, None), device("COM11", Some(0x10C4), Some(0xEA60))],
        );
        assert!(choice.port.is_none());
        assert!(choice.warning.is_none());
    }

    #[test]
    fn selects_single_matching_teensy_vid() {
        let choice = choose_deploy_port(
            None,
            Platform::Teensy,
            None,
            vec![
                device("COM22", Some(0x303A), Some(0x1001)),
                device("COM21", Some(0x16C0), Some(0x0489)),
            ],
        );
        assert_eq!(choice.port.as_deref(), Some("COM21"));
        assert!(choice.warning.is_none());
    }

    #[test]
    fn multiple_matches_pick_sorted_port_and_warn() {
        let choice = choose_deploy_port(
            None,
            Platform::Espressif32,
            None,
            vec![
                device("COM22", Some(0x303A), Some(0x1001)),
                device("COM9", Some(0x303A), Some(0x1001)),
            ],
        );
        assert_eq!(choice.port.as_deref(), Some("COM22"));
        assert!(choice
            .warning
            .unwrap()
            .contains("multiple serial ports matched"));
    }

    #[test]
    fn no_match_falls_back_sorted_and_warns() {
        let choice = choose_deploy_port(
            None,
            Platform::Teensy,
            None,
            vec![
                device("COM22", Some(0x303A), Some(0x1001)),
                device("COM9", Some(0x303A), Some(0x1001)),
            ],
        );
        assert_eq!(choice.port.as_deref(), Some("COM22"));
        assert!(choice.warning.unwrap().contains("no serial port matched"));
    }

    #[test]
    fn board_vid_augments_family_defaults() {
        let overrides = HashMap::new();
        let mut board =
            BoardConfig::from_board_id_or_default("seeed_xiao_esp32s3", "", &overrides, None);
        board.vid = Some("0x2886".to_string());
        let choice = choose_deploy_port(
            None,
            Platform::Espressif32,
            Some(&board),
            vec![device("COM7", Some(0x2886), Some(0x0056))],
        );
        assert_eq!(choice.port.as_deref(), Some("COM7"));
    }
}
