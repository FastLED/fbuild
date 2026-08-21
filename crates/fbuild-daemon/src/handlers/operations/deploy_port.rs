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
    choose_deploy_port_with_profile_lookup(
        requested,
        platform,
        board_id,
        board,
        devices,
        fbuild_core::usb::profiles::board_profile,
    )
}

fn choose_deploy_port_with_profile_lookup(
    requested: Option<String>,
    platform: Platform,
    board_id: Option<&str>,
    board: Option<&BoardConfig>,
    devices: Vec<DeviceState>,
    profile_lookup: impl FnOnce(&str) -> Option<fbuild_core::usb::profiles::BoardUsbProfile>,
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
        let expected_generation = rp_generation_for(board);
        let board_profile = rp_board_profile_id(board_id, board).and_then(profile_lookup);
        let (matches, unhealthy) =
            partition_rp_candidates_for_board(devices, board_profile.as_ref(), expected_generation);
        return rp_deploy_choice(matches, unhealthy);
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

fn rp_board_profile_id<'a>(
    board_id: Option<&'a str>,
    board: Option<&'a BoardConfig>,
) -> Option<&'a str> {
    board.map(BoardConfig::registry_board_id).or(board_id)
}

/// Split connected Raspberry Pi family matches into deploy-eligible
/// candidates and known-unhealthy records (FastLED/fbuild#1147). Phantom and
/// present-problem devnodes stay visible for diagnostics but are never
/// auto-selected: touching a stale COM name is guaranteed to fail, while the
/// BOOTSEL-volume path can still deploy.
fn partition_rp_candidates(
    devices: Vec<DeviceState>,
    mut family_match: impl FnMut(u16, u16) -> bool,
) -> (Vec<PortCandidate>, Vec<String>) {
    let mut matches = Vec::new();
    let mut unhealthy = Vec::new();
    for device in devices.into_iter().filter(|device| device.is_connected) {
        let matched = device
            .vid
            .zip(device.pid)
            .is_some_and(|(vid, pid)| family_match(vid, pid));
        if !matched {
            continue;
        }
        if device.port_health.is_known_unhealthy() {
            unhealthy.push(describe_unhealthy_device(&device));
            continue;
        }
        matches.push(PortCandidate {
            port: device.port,
            vid: device.vid,
            pid: device.pid,
            description: device.description,
        });
    }
    matches.sort_by(|a, b| a.port.cmp(&b.port));
    (matches, unhealthy)
}

/// Prefer the requested board's exact runtime identity. Generation matching
/// remains available for callers whose board profile has not been published
/// yet, but must not make W and non-W variants interchangeable once exact
/// identities exist.
fn partition_rp_candidates_for_board(
    devices: Vec<DeviceState>,
    board_profile: Option<&fbuild_core::usb::profiles::BoardUsbProfile>,
    expected_generation: RpGeneration,
) -> (Vec<PortCandidate>, Vec<String>) {
    partition_rp_candidates(devices, |vid, pid| {
        let profiles = fbuild_core::usb::profiles::profiles_for(vid, pid);
        rp_runtime_identity_matches(board_profile, vid, pid, &profiles, expected_generation)
    })
}

fn rp_runtime_identity_matches(
    board_profile: Option<&fbuild_core::usb::profiles::BoardUsbProfile>,
    vid: u16,
    pid: u16,
    profiles: &[fbuild_core::usb::profiles::UsbTransportProfile],
    expected_generation: RpGeneration,
) -> bool {
    match board_profile.and_then(board_runtime_identities) {
        Some(identities) => identities
            .iter()
            .any(|identity| identity_matches(identity, vid, pid)),
        None => rp_profiles_match_generation(profiles, expected_generation),
    }
}

fn board_runtime_identities(
    profile: &fbuild_core::usb::profiles::BoardUsbProfile,
) -> Option<&[String]> {
    profile
        .identities
        .get("runtime")
        .filter(|identities| !identities.is_empty())
        .map(Vec::as_slice)
}

fn describe_unhealthy_device(device: &DeviceState) -> String {
    let problem = device
        .port_health
        .problem_code()
        .map(|code| format!("; problem code {code}"))
        .unwrap_or_default();
    let instance = device
        .instance_id
        .as_deref()
        .map(|value| format!("; instance {value}"))
        .unwrap_or_default();
    format!(
        "{} (health {}{problem}{instance})",
        device.port,
        device.port_health.label()
    )
}

/// Final Raspberry Pi deploy-port decision from the partitioned candidates.
fn rp_deploy_choice(matches: Vec<PortCandidate>, unhealthy: Vec<String>) -> DeployPortChoice {
    let unhealthy_note = (!unhealthy.is_empty()).then(|| {
        format!(
            "excluded known-unhealthy Raspberry Pi CDC record(s) from deploy selection: {}; they stay visible to `fbuild port scan`, and deploy continues via the BOOTSEL volume path",
            unhealthy.join(", ")
        )
    });
    if matches.len() == 1 {
        log_connect("deploy", &matches[0]);
        return DeployPortChoice {
            port: Some(matches[0].port.clone()),
            warning: unhealthy_note,
        };
    }
    if matches.len() > 1 {
        let ambiguity = format!(
            "multiple FastLED/boards-identified Raspberry Pi CDC ports are connected: {}; pass -p/--port to select the deployment target",
            format_candidates(matches.iter())
        );
        return DeployPortChoice {
            port: None,
            warning: Some(match unhealthy_note {
                Some(note) => format!("{ambiguity}; {note}"),
                None => ambiguity,
            }),
        };
    }
    DeployPortChoice {
        port: None,
        warning: unhealthy_note,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RpGeneration {
    Rp2040,
    Rp2350,
}

impl RpGeneration {
    fn family(self) -> &'static str {
        match self {
            RpGeneration::Rp2040 => "rp2040",
            RpGeneration::Rp2350 => "rp2350",
        }
    }
}

pub(super) fn rp_generation_for(board: Option<&BoardConfig>) -> RpGeneration {
    board
        .map(|board| board.mcu.to_ascii_lowercase())
        .filter(|mcu| mcu.starts_with("rp2350"))
        .map_or(RpGeneration::Rp2040, |_| RpGeneration::Rp2350)
}

pub(super) fn rp_profiles_match_generation(
    profiles: &[fbuild_core::usb::profiles::UsbTransportProfile],
    expected: RpGeneration,
) -> bool {
    use fbuild_core::usb::profiles::{UsbDeviceRole, UsbPurpose};

    profiles.iter().any(|profile| {
        profile.purpose == UsbPurpose::Runtime
            && profile.role == UsbDeviceRole::RuntimeCdc
            && profile.family.as_deref() == Some(expected.family())
    })
}

/// Match a BOOTSEL UF2 bootloader profile of the expected RP generation
/// (FastLED/fbuild#1152: identifies the stock-ROM composite whose problem
/// interface may carry a typed recovery request).
pub(super) fn rp_bootloader_profiles_match_generation(
    profiles: &[fbuild_core::usb::profiles::UsbTransportProfile],
    expected: RpGeneration,
) -> bool {
    use fbuild_core::usb::profiles::{UsbDeviceRole, UsbPurpose};

    profiles.iter().any(|profile| {
        profile.purpose == UsbPurpose::Bootloader
            && profile.role == UsbDeviceRole::BootloaderUf2
            && profile.family.as_deref() == Some(expected.family())
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
        .as_ref()
        .and_then(board_runtime_identities)
        .is_some_and(|identities| {
            identities
                .iter()
                .any(|identity| identity_matches(identity, vid, pid))
        })
}

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
    use std::collections::{BTreeMap, HashMap};

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
            port_health: fbuild_serial::ports::PortHealth::Unknown,
            instance_id: None,
            parent_instance_id: None,
            location_paths: Vec::new(),
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

    fn board_profile(
        board_id: &str,
        runtime_identities: &[&str],
    ) -> fbuild_core::usb::profiles::BoardUsbProfile {
        fbuild_core::usb::profiles::BoardUsbProfile {
            board_id: board_id.to_string(),
            identities: BTreeMap::from([(
                "runtime".to_string(),
                runtime_identities
                    .iter()
                    .map(|identity| (*identity).to_string())
                    .collect(),
            )]),
            aliases: Vec::new(),
            primary_compile_identity: None,
        }
    }

    #[test]
    fn raspberry_pi_runtime_selection_uses_exact_board_profile() {
        for (board_id, expected_pid, wrong_pid, generation) in [
            ("rpipico", 0x000A, 0xF00A, RpGeneration::Rp2040),
            ("rpipicow", 0xF00A, 0x000A, RpGeneration::Rp2040),
            ("rpipico2", 0x000F, 0xF00F, RpGeneration::Rp2350),
            ("rpipico2w", 0xF00F, 0x000F, RpGeneration::Rp2350),
        ] {
            let identity = format!("2e8a:{expected_pid:04x}");
            let profile = board_profile(board_id, &[&identity]);
            let family = generation.family();
            let same_generation = runtime_profile(Some("raspberrypi"), Some(family), false);
            assert!(
                !rp_runtime_identity_matches(
                    Some(&profile),
                    0x2E8A,
                    wrong_pid,
                    std::slice::from_ref(&same_generation),
                    generation,
                ),
                "{board_id} must not fall back to generation matching",
            );
            let (matches, unhealthy) = partition_rp_candidates_for_board(
                vec![
                    device("COM17", Some(0x2E8A), Some(wrong_pid)),
                    device("COM18", Some(0x2E8A), Some(expected_pid)),
                ],
                Some(&profile),
                generation,
            );

            assert!(unhealthy.is_empty(), "{board_id}");
            assert_eq!(matches.len(), 1, "{board_id}");
            assert_eq!(matches[0].port, "COM18", "{board_id}");

            let (wrong_variant, _) = partition_rp_candidates_for_board(
                vec![device("COM17", Some(0x2E8A), Some(wrong_pid))],
                Some(&profile),
                generation,
            );
            assert!(wrong_variant.is_empty(), "{board_id}");
        }
    }

    #[test]
    fn raspberry_pi_runtime_selection_falls_back_when_profile_is_unavailable() {
        let pico = runtime_profile(Some("raspberrypi"), Some("rp2040"), false);

        assert!(rp_runtime_identity_matches(
            None,
            0x2E8A,
            0x000A,
            std::slice::from_ref(&pico),
            RpGeneration::Rp2040,
        ));
        assert!(!rp_runtime_identity_matches(
            None,
            0x2E8A,
            0x000A,
            std::slice::from_ref(&pico),
            RpGeneration::Rp2350,
        ));

        let missing_runtime = board_profile("unpublished-runtime", &[]);
        assert!(rp_runtime_identity_matches(
            Some(&missing_runtime),
            0x2E8A,
            0x000A,
            std::slice::from_ref(&pico),
            RpGeneration::Rp2040,
        ));
    }

    #[test]
    fn raspberry_pi_profile_lookup_uses_canonical_config_aliases() {
        for (alias, canonical, expected_pid, wrong_pid, generation) in [
            ("pico", "rpipico", 0x000A, 0xF00A, RpGeneration::Rp2040),
            ("rpipico", "rpipico", 0x000A, 0xF00A, RpGeneration::Rp2040),
            ("picow", "rpipicow", 0xF00A, 0x000A, RpGeneration::Rp2040),
            ("rpipicow", "rpipicow", 0xF00A, 0x000A, RpGeneration::Rp2040),
            ("pico2", "rpipico2", 0x000F, 0xF00F, RpGeneration::Rp2350),
            ("rpipico2", "rpipico2", 0x000F, 0xF00F, RpGeneration::Rp2350),
            ("pico2w", "rpipico2w", 0xF00F, 0x000F, RpGeneration::Rp2350),
            ("pico2wh", "rpipico2w", 0xF00F, 0x000F, RpGeneration::Rp2350),
            (
                "rpipico2w",
                "rpipico2w",
                0xF00F,
                0x000F,
                RpGeneration::Rp2350,
            ),
            (
                "rpipico2wh",
                "rpipico2w",
                0xF00F,
                0x000F,
                RpGeneration::Rp2350,
            ),
        ] {
            let board = BoardConfig::from_board_id(alias, &HashMap::new()).unwrap();
            assert_eq!(rp_generation_for(Some(&board)), generation, "{alias}");
            let identity = format!("2e8a:{expected_pid:04x}");
            let profile = board_profile(canonical, &[&identity]);
            let lookup = |lookup_id: &str| {
                assert_eq!(lookup_id, canonical, "{alias}");
                Some(profile.clone())
            };

            let choice = choose_deploy_port_with_profile_lookup(
                None,
                Platform::RaspberryPi,
                Some(alias),
                Some(&board),
                vec![
                    device("COM17", Some(0x2E8A), Some(wrong_pid)),
                    device("COM18", Some(0x2E8A), Some(expected_pid)),
                ],
                lookup,
            );
            assert_eq!(choice.port.as_deref(), Some("COM18"), "{alias}");

            let wrong_variant = choose_deploy_port_with_profile_lookup(
                None,
                Platform::RaspberryPi,
                Some(alias),
                Some(&board),
                vec![device("COM17", Some(0x2E8A), Some(wrong_pid))],
                lookup,
            );
            assert!(wrong_variant.port.is_none(), "{alias}");
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

    fn phantom_device(port: &str, instance_id: &str) -> DeviceState {
        let mut state = device(port, Some(0x2E8A), Some(0x000A));
        state.port_health = fbuild_serial::ports::PortHealth::Phantom {
            problem_code: Some(45),
            status: None,
        };
        state.instance_id = Some(instance_id.to_string());
        state
    }

    #[test]
    fn phantom_rp2040_cdc_is_never_auto_selected() {
        let (matches, unhealthy) = partition_rp_candidates(
            vec![phantom_device(
                "COM12",
                "USB\\VID_2E8A&PID_000A\\5303284720C4641C",
            )],
            |_, _| true,
        );
        assert!(matches.is_empty());
        let choice = rp_deploy_choice(matches, unhealthy);
        assert!(choice.port.is_none());
        let warning = choice.warning.expect("the exclusion must be diagnosed");
        assert!(warning.contains("COM12"), "missing port: {warning}");
        assert!(
            warning.contains("health phantom"),
            "missing health: {warning}"
        );
        assert!(
            warning.contains("problem code 45"),
            "missing code: {warning}"
        );
        assert!(
            warning.contains("USB\\VID_2E8A&PID_000A\\5303284720C4641C"),
            "missing instance: {warning}"
        );
        assert!(
            warning.contains("BOOTSEL volume path"),
            "missing path: {warning}"
        );
    }

    #[test]
    fn healthy_rp2040_cdc_is_selected_while_phantom_history_is_reported() {
        let mut healthy = device("COM27", Some(0x2E8A), Some(0x000A));
        healthy.port_health = fbuild_serial::ports::PortHealth::HealthyPresent;
        let (matches, unhealthy) = partition_rp_candidates(
            vec![
                phantom_device("COM12", "USB\\VID_2E8A&PID_000A\\5303284720C4641C"),
                healthy,
            ],
            |_, _| true,
        );
        let choice = rp_deploy_choice(matches, unhealthy);
        assert_eq!(choice.port.as_deref(), Some("COM27"));
        let warning = choice
            .warning
            .expect("the exclusion must still be diagnosed");
        assert!(warning.contains("COM12"));
    }

    #[test]
    fn present_problem_rp2040_cdc_is_excluded_from_selection() {
        let mut broken = device("COM12", Some(0x2E8A), Some(0x000A));
        broken.port_health = fbuild_serial::ports::PortHealth::PresentProblem {
            problem_code: 31,
            status: None,
        };
        let (matches, unhealthy) = partition_rp_candidates(vec![broken], |_, _| true);
        assert!(matches.is_empty());
        assert_eq!(unhealthy.len(), 1);
        assert!(unhealthy[0].contains("health present-problem"));
        assert!(unhealthy[0].contains("problem code 31"));
    }

    #[test]
    fn unknown_health_rp2040_cdc_remains_eligible() {
        let (matches, unhealthy) =
            partition_rp_candidates(vec![device("COM5", Some(0x2E8A), Some(0x000A))], |_, _| {
                true
            });
        assert_eq!(matches.len(), 1);
        assert!(unhealthy.is_empty());
        assert_eq!(
            rp_deploy_choice(matches, unhealthy).port.as_deref(),
            Some("COM5")
        );
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
