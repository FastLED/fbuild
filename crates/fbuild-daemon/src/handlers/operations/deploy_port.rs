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
    board_id: Option<&str>,
    board: Option<&BoardConfig>,
    devices: Vec<DeviceState>,
) -> DeployPortChoice {
    if requested.is_some() {
        return DeployPortChoice {
            port: requested,
            warning: None,
        };
    }

    // A stock/blank Pico may have no CDC port, but a previously flashed Pico
    // needs its catalogue-identified CDC port passed through for the 1200-bps
    // reset. Never select by a built-in VID or fall back to an unrelated COM
    // port: FastLED/boards data is the sole identity source.
    if platform == Platform::RaspberryPi {
        let expected_generation = board
            .map(|board| board.mcu.to_ascii_lowercase())
            .filter(|mcu| mcu.starts_with("rp2350"))
            .map_or(RpGeneration::Rp2040, |_| RpGeneration::Rp2350);
        let mut matches: Vec<_> = devices
            .into_iter()
            .filter(|device| device.is_connected)
            .filter_map(|device| {
                let matches = device.vid.zip(device.pid).is_some_and(|(vid, pid)| {
                    rp_profiles_match_generation(
                        &fbuild_core::usb::profiles::profiles_for(vid, pid),
                        expected_generation,
                    )
                });
                matches.then_some(PortCandidate {
                    port: device.port,
                    vid: device.vid,
                    pid: device.pid,
                    description: device.description,
                })
            })
            .collect();
        matches.sort_by(|a, b| a.port.cmp(&b.port));
        if matches.len() == 1 {
            log_connect("deploy", &matches[0]);
            return DeployPortChoice {
                port: Some(matches[0].port.clone()),
                warning: None,
            };
        }
        if matches.len() > 1 {
            return DeployPortChoice {
                port: None,
                warning: Some(format!(
                    "multiple FastLED/boards-identified Raspberry Pi CDC ports are connected: {}; pass -p/--port to select the deployment target",
                    format_candidates(matches.iter())
                )),
            };
        }
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
        .filter(|candidate| {
            candidate.vid.zip(candidate.pid).is_some_and(|(vid, pid)| {
                device_matches_deploy_target(platform, board_id, board, vid, pid)
            })
        })
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
                "multiple serial ports matched FastLED/boards deploy profiles; selected {} deterministically from {}; pass -p/--port to choose explicitly",
                selected.port,
                format_candidates(matches.iter().copied()),
            )),
        }
    } else if !candidates.is_empty() {
        DeployPortChoice {
            port: None,
            warning: Some(format!(
                "no serial port matched a FastLED/boards deploy profile for {platform:?}; connected candidates: {}; pass -p/--port to choose explicitly or publish the missing board identity in FastLED/boards",
                format_candidates(candidates.iter()),
            )),
        }
    } else {
        DeployPortChoice {
            port: None,
            warning: Some(format!("no serial ports found for {platform:?}")),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RpGeneration {
    Rp2040,
    Rp2350,
}

fn rp_profiles_match_generation(
    profiles: &[fbuild_core::usb::profiles::UsbTransportProfile],
    expected: RpGeneration,
) -> bool {
    use fbuild_core::usb::profiles::{UsbDeviceRole, UsbPurpose};

    let family = match expected {
        RpGeneration::Rp2040 => "rp2040",
        RpGeneration::Rp2350 => "rp2350",
    };
    profiles.iter().any(|profile| {
        profile.purpose == UsbPurpose::Runtime
            && profile.role == UsbDeviceRole::RuntimeCdc
            && profile.family.as_deref() == Some(family)
    })
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

fn device_matches_deploy_target(
    platform: Platform,
    board_id: Option<&str>,
    board: Option<&BoardConfig>,
    vid: u16,
    pid: u16,
) -> bool {
    #[cfg(test)]
    {
        let _ = (board_id, board);
        test_device_matches_deploy_target(platform, vid, pid)
    }
    #[cfg(not(test))]
    {
        if board_id.is_some_and(|id| board_runtime_identity_matches(id, vid, pid)) {
            return true;
        }
        let profiles = fbuild_core::usb::profiles::profiles_for(vid, pid);
        profiles_match_deploy_target(platform, board, &profiles)
    }
}

#[cfg(test)]
fn test_device_matches_deploy_target(platform: Platform, vid: u16, pid: u16) -> bool {
    match platform {
        Platform::Teensy => (vid, pid) == (0x16C0, 0x0489),
        Platform::Espressif32 => (vid, pid) == (0x303A, 0x1001),
        _ => false,
    }
}

#[cfg(not(test))]
fn board_runtime_identity_matches(board_id: &str, vid: u16, pid: u16) -> bool {
    fbuild_core::usb::profiles::board_profile(board_id)
        .and_then(|profile| profile.identities.get("runtime").cloned())
        .is_some_and(|identities| {
            identities
                .iter()
                .any(|identity| identity_matches(identity, vid, pid))
        })
}

#[cfg(not(test))]
fn identity_matches(identity: &str, vid: u16, pid: u16) -> bool {
    let Some((expected_vid, expected_pid)) = identity.split_once(':') else {
        return false;
    };
    u16::from_str_radix(expected_vid, 16).ok() == Some(vid)
        && (expected_pid == "*" || u16::from_str_radix(expected_pid, 16).ok() == Some(pid))
}

fn profiles_match_deploy_target(
    platform: Platform,
    _board: Option<&BoardConfig>,
    profiles: &[fbuild_core::usb::profiles::UsbTransportProfile],
) -> bool {
    use fbuild_core::usb::profiles::{UsbDeviceRole, UsbPurpose};

    profiles.iter().any(|profile| {
        if profile.purpose != UsbPurpose::Runtime
            || !matches!(
                profile.role,
                UsbDeviceRole::RuntimeCdc | UsbDeviceRole::UsbUartBridge
            )
        {
            return false;
        }
        let profile_platform = profile.platform.as_deref();
        match platform {
            Platform::Teensy => {
                profile_platform == Some("teensy") || profile.family.as_deref() == Some("teensy")
            }
            Platform::Espressif32 => {
                profile_platform == Some("espressif32")
                    || profile.role == UsbDeviceRole::UsbUartBridge
            }
            Platform::AtmelAvr | Platform::AtmelMegaAvr => {
                profile_platform == Some("arduino") || profile.role == UsbDeviceRole::UsbUartBridge
            }
            Platform::NxpLpc => profile_platform == Some("nxplpc"),
            _ => false,
        }
    })
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

    fn runtime_profile(
        platform: Option<&str>,
        family: Option<&str>,
        bridge: bool,
    ) -> fbuild_core::usb::profiles::UsbTransportProfile {
        use fbuild_core::usb::profiles::{
            UsbDeviceRole, UsbIdentityMatch, UsbProfileProvenance, UsbPurpose, UsbTransportProfile,
        };
        UsbTransportProfile {
            identity_match: UsbIdentityMatch {
                vid: "feed".to_string(),
                pid: Some("c0de".to_string()),
                pid_mask: None,
            },
            purpose: UsbPurpose::Runtime,
            role: if bridge {
                UsbDeviceRole::UsbUartBridge
            } else {
                UsbDeviceRole::RuntimeCdc
            },
            transport: if bridge { "serial" } else { "usb" }.to_string(),
            reset: "hardware".to_string(),
            handoff: "reconnect".to_string(),
            platform: platform.map(str::to_string),
            family: family.map(str::to_string),
            generation: None,
            interface: Some(if bridge { "uart" } else { "cdc" }.to_string()),
            provenance: UsbProfileProvenance {
                source_url: "test://fixture".to_string(),
                source_revision: "a".repeat(40),
                source_class: "test".to_string(),
            },
            priority: 100,
            allow_ambiguous: false,
        }
    }

    #[test]
    fn deploy_target_matching_uses_profile_semantics() {
        let teensy = runtime_profile(Some("teensy"), Some("teensy"), false);
        assert!(profiles_match_deploy_target(
            Platform::Teensy,
            None,
            std::slice::from_ref(&teensy)
        ));
        assert!(!profiles_match_deploy_target(
            Platform::Espressif32,
            None,
            std::slice::from_ref(&teensy)
        ));

        let bridge = runtime_profile(None, Some("cp210x"), true);
        assert!(profiles_match_deploy_target(
            Platform::Espressif32,
            None,
            std::slice::from_ref(&bridge)
        ));
        assert!(profiles_match_deploy_target(
            Platform::AtmelAvr,
            None,
            std::slice::from_ref(&bridge)
        ));
    }

    #[test]
    fn explicit_port_wins() {
        let choice = choose_deploy_port(
            Some("COM21".to_string()),
            Platform::Teensy,
            None,
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
            None,
            vec![
                device("COM1", None, None),
                device("COM11", Some(0x10C4), Some(0xEA60)),
            ],
        );
        assert!(choice.port.is_none());
        assert!(choice.warning.is_none());
    }

    #[test]
    fn raspberry_pi_identity_is_catalogue_driven() {
        let pico = runtime_profile(Some("raspberrypi"), Some("rp2040"), false);
        let pico2 = runtime_profile(Some("raspberrypi"), Some("rp2350"), false);
        assert!(rp_profiles_match_generation(
            std::slice::from_ref(&pico),
            RpGeneration::Rp2040
        ));
        assert!(!rp_profiles_match_generation(
            std::slice::from_ref(&pico2),
            RpGeneration::Rp2040
        ));
        assert!(rp_profiles_match_generation(
            std::slice::from_ref(&pico2),
            RpGeneration::Rp2350
        ));
        assert!(!rp_profiles_match_generation(&[], RpGeneration::Rp2040));
    }

    #[test]
    fn selects_single_matching_teensy_vid() {
        let choice = choose_deploy_port(
            None,
            Platform::Teensy,
            None,
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
            None,
            vec![
                device("COM22", Some(0x303A), Some(0x1001)),
                device("COM9", Some(0x303A), Some(0x1001)),
            ],
        );
        assert_eq!(choice.port.as_deref(), Some("COM22"));
        assert!(
            choice
                .warning
                .unwrap()
                .contains("multiple serial ports matched")
        );
    }

    #[test]
    fn no_match_refuses_to_guess_and_warns() {
        let choice = choose_deploy_port(
            None,
            Platform::Teensy,
            None,
            None,
            vec![
                device("COM22", Some(0x303A), Some(0x1001)),
                device("COM9", Some(0x303A), Some(0x1001)),
            ],
        );
        assert!(choice.port.is_none());
        assert!(choice.warning.unwrap().contains("no serial port matched"));
    }

    #[test]
    fn unknown_board_identity_is_not_guessed() {
        let overrides = HashMap::new();
        let board =
            BoardConfig::from_board_id_or_default("seeed_xiao_esp32s3", "", &overrides, None);
        let choice = choose_deploy_port(
            None,
            Platform::Espressif32,
            Some("seeed_xiao_esp32s3"),
            Some(&board),
            vec![device("COM7", Some(0x2886), Some(0x0056))],
        );
        assert!(choice.port.is_none());
        assert!(choice.warning.unwrap().contains("FastLED/boards"));
    }
}
