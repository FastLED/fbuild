//! `fbuild port doctor` — explain what is actually wrong with a serial port.
//!
//! FastLED/fbuild#1279. `fbuild port scan` answers "what does the OS list?".
//! This answers the question people actually have when a board will not
//! deploy: **is it not plugged in, or is it broken?** Those look identical in
//! a scan and need opposite responses, and conflating them cost a full
//! investigation in FastLED/FastLED#3864.
//!
//! Strictly read-only. It never elevates, never prompts, and never mutates
//! host state — a diagnostic that can change things is one people stop
//! trusting to run.

use fbuild_core::{FbuildError, Result};
use fbuild_serial::ports::{DetectedPort, UsbProblemDevice};

/// One port's diagnosis, decoupled from rendering so the verdict logic is
/// testable without a live device tree.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct PortDiagnosis {
    pub port: String,
    /// `Some(true)` attached, `Some(false)` a stale record, `None` unknowable.
    pub presence: Option<bool>,
    pub health: String,
    pub problem_code: Option<u32>,
    pub instance_id: Option<String>,
    pub parent_instance_id: Option<String>,
    /// Whether any USB ancestor of this port may be powered down by Windows.
    /// `None` when unknown — never silently `false`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suspend_allowed: Option<bool>,
    /// Seconds since the devnode was last seen on the bus. Only populated for
    /// a single-port query — see `query_last_seen_secs` for why.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen_secs_ago: Option<i64>,
}

/// What the diagnosis means and what to do about it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Verdict {
    pub summary: String,
    pub remedy: String,
    /// True when a human has to physically do something. Kept explicit so a
    /// caller never advertises an automated recovery it cannot perform.
    pub needs_hands: bool,
}

/// Plain-English meaning for the Config Manager problem codes that actually
/// show up on a bench. Unknown codes render as the bare number rather than a
/// guess.
pub fn problem_code_meaning(code: u32) -> Option<&'static str> {
    Some(match code {
        10 => "device cannot start",
        22 => "device is disabled",
        28 => "drivers are not installed",
        43 => "device descriptor request failed — the device stopped responding on the bus",
        45 => "not currently connected (historical record)",
        _ => return None,
    })
}

pub fn diagnose(port: &DetectedPort, power_rows: &[(String, bool)]) -> PortDiagnosis {
    PortDiagnosis {
        port: port.info.port_name.clone(),
        presence: port.health.is_present(),
        health: port.health.label().to_string(),
        problem_code: port.health.problem_code(),
        instance_id: port.instance_id.clone(),
        parent_instance_id: port.parent_instance_id.clone(),
        suspend_allowed: suspend_for_ancestors(power_rows, &port.ancestor_instance_ids),
        last_seen_secs_ago: None,
    }
}

pub fn verdict(diagnosis: &PortDiagnosis) -> Verdict {
    match (diagnosis.presence, diagnosis.problem_code) {
        // The case this command exists for. A phantom record is not a fault.
        (Some(false), _) => Verdict {
            summary: "not attached — this is a stale registry record, not a fault".to_string(),
            remedy: "plug the board in, then re-run `fbuild port scan`".to_string(),
            needs_hands: true,
        },
        // Attached but the descriptor exchange failed. A bus reset will not
        // fix this; it needs an actual VBUS cycle.
        (Some(true), Some(43)) => Verdict {
            summary: "attached but failed USB enumeration (device descriptor request failed)"
                .to_string(),
            remedy: "unplug and replug the board — a bus reset is not a VBUS cycle, so \
                     disable/enable and hub restarts will not recover it. If it also fails \
                     to enumerate in BOOTSEL (which runs the bootrom's own USB stack), the \
                     fault is hardware or cabling, not firmware"
                .to_string(),
            needs_hands: true,
        },
        (Some(true), Some(code)) => Verdict {
            summary: match problem_code_meaning(code) {
                Some(meaning) => format!("attached but reporting a problem: {meaning}"),
                None => format!("attached but reporting problem code {code}"),
            },
            remedy: "check Device Manager for this devnode; unplug and replug to re-enumerate"
                .to_string(),
            needs_hands: true,
        },
        (Some(true), None) => Verdict {
            summary: "attached and healthy".to_string(),
            remedy: String::new(),
            needs_hands: false,
        },
        (None, _) => Verdict {
            summary: "host cannot report presence for this endpoint (normal off Windows)"
                .to_string(),
            remedy: String::new(),
            needs_hands: false,
        },
    }
}

/// Windows power-setting GUIDs: USB subgroup, then selective suspend.
pub const USB_SUBGROUP_GUID: &str = "2a737441-1930-4402-8d77-b2bebba308a3";
pub const SELECTIVE_SUSPEND_GUID: &str = "48e6b7a6-50f5-4782-a5d4-53bb8f07e226";

/// Parse `powercfg /q` output for whether USB selective suspend is enabled.
///
/// `None` when the output cannot be interpreted — callers must treat that as
/// "no opinion", never as "disabled", or the report would quietly clear a host
/// that was never checked.
pub fn parse_selective_suspend(powercfg_output: &str) -> Option<bool> {
    let line = powercfg_output
        .lines()
        .find(|l| l.contains("Current AC Power Setting Index"))?;
    let hex = line.rsplit_once(':')?.1.trim().trim_start_matches("0x");
    u32::from_str_radix(hex, 16).ok().map(|v| v != 0)
}

/// Parse `MSPower_DeviceEnable` rows rendered as `instance|Enable`.
///
/// `Enable = True` means Windows is *allowed* to power the device down. The
/// WMI `InstanceName` carries a `_0` suffix that the PnP instance ID does
/// not, so it is trimmed here rather than at every call site.
pub fn parse_device_power_rows(output: &str) -> Vec<(String, bool)> {
    output
        .lines()
        .filter_map(|line| {
            let (instance, enable) = line.trim().split_once('|')?;
            let instance = instance.trim();
            // `strip_suffix`, not `trim_end_matches`: the latter strips
            // repeatedly, so an instance legitimately ending `_0_0` would lose
            // both and stop matching its devnode.
            let instance = instance.strip_suffix("_0").unwrap_or(instance);
            let instance = instance.to_ascii_uppercase();
            if instance.is_empty() {
                return None;
            }
            // An unrecognised value is *no data*, not "cannot be powered off".
            // Treating it as false would quietly clear a device nobody checked
            // — the same trap as reporting unknown suspend state as disabled.
            let enable = match enable.trim() {
                v if v.eq_ignore_ascii_case("true") => true,
                v if v.eq_ignore_ascii_case("false") => false,
                _ => return None,
            };
            Some((instance, enable))
        })
        .collect()
}

/// Whether any USB ancestor of this port may be powered down.
///
/// Takes the **whole ancestor chain**, not just the immediate parent. For a
/// composite device the immediate parent is the device itself; the nodes
/// carrying power policy are hubs further up. Matching only one hop silently
/// reports `None` for every real port — which is exactly what happened on
/// this bench before the chain was threaded through.
///
/// `None` when nothing matched: silence beats a false "fine".
pub fn suspend_for_ancestors(power_rows: &[(String, bool)], ancestors: &[String]) -> Option<bool> {
    let mut matched = false;
    for ancestor in ancestors {
        let ancestor = ancestor.to_ascii_uppercase();
        for (instance, can_power_off) in power_rows {
            if ancestor == *instance {
                matched = true;
                if *can_power_off {
                    return Some(true);
                }
            }
        }
    }
    matched.then_some(false)
}

/// Parse `instance|unix_seconds` rows into a last-seen timestamp.
///
/// The PowerShell side emits `ToUnixTimeSeconds()` rather than a formatted
/// date on purpose: a rendered date is locale-dependent and would parse
/// differently on a non-English host.
pub fn parse_last_seen_rows(output: &str) -> Option<i64> {
    output
        .lines()
        .filter_map(|line| line.trim().rsplit_once('|'))
        .filter_map(|(_, secs)| secs.trim().parse::<i64>().ok())
        .next()
}

/// Render an elapsed duration as a short human age: `45s`, `12m`, `3h`, `6d`.
///
/// Deliberately coarse — the reader only needs "moments ago" versus "days
/// ago" to tell a live board from a stale record.
/// The whole "last seen" phrase, so the negative case cannot render as the
/// nonsense "last seen in the future ago". Clock skew and a devnode stamped
/// slightly ahead of the host are both real, so the branch has to exist.
pub fn format_last_seen(secs: i64) -> String {
    if secs < 0 {
        return "a timestamp in the future (host clock skew?)".to_string();
    }
    format!("{} ago", format_age(secs))
}

pub fn format_age(secs: i64) -> String {
    if secs < 0 {
        return "0s".to_string();
    }
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

/// Host-level section describing USB selective suspend.
///
/// Why the report cares: Windows may power a port down mid-session, and a
/// board that does not resume cleanly returns as code 43 — whose stale COM
/// record then reads `health=phantom`. That is the same signature as an
/// unplugged board, so naming the setting here keeps the reader from chasing
/// a phantom that host power management actually caused.
pub fn render_suspend_section(enabled: Option<bool>) -> String {
    match enabled {
        Some(true) => format!(
            "host\n  suspend    ENABLED in the active power plan\n  \
             verdict    Windows may power a port down mid-session; a board that does not\n             \
             resume returns as code 43 and its record then reads health=phantom\n  \
             remedy     from an elevated shell: powercfg -setacvalueindex SCHEME_CURRENT \
             {USB_SUBGROUP_GUID} {SELECTIVE_SUSPEND_GUID} 0\n"
        ),
        Some(false) => "host\n  suspend    disabled in the active power plan\n".to_string(),
        None => String::new(),
    }
}

/// Render the human report.
///
/// `problems` are present USB devices with a fault that fbuild could **not**
/// associate with any listed port. They are deliberately rendered in their own
/// section: attributing an unmatched code-43 node to a nearby board is exactly
/// the mistake that produced the "wedged board" misdiagnosis in #3864, when
/// the failing device turned out to be a different board on a different hub.
pub fn render_report(diagnoses: &[PortDiagnosis], problems: &[UsbProblemDevice]) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();

    if diagnoses.is_empty() {
        out.push_str("no serial ports visible\n");
    }

    for d in diagnoses {
        let v = verdict(d);
        let _ = writeln!(out, "{}", d.port);
        let presence = match d.presence {
            Some(true) => "attached",
            Some(false) => "NOT PRESENT",
            None => "unknown",
        };
        let _ = writeln!(out, "  presence   {presence}");
        if let Some(secs) = d.last_seen_secs_ago {
            let _ = writeln!(out, "  last seen  {}", format_last_seen(secs));
        }
        let mut health_line = format!("  health     {}", d.health);
        if let Some(code) = d.problem_code {
            match problem_code_meaning(code) {
                Some(meaning) => {
                    let _ = write!(health_line, " (problem {code}: {meaning})");
                }
                None => {
                    let _ = write!(health_line, " (problem {code})");
                }
            }
        }
        let _ = writeln!(out, "{health_line}");
        if let Some(parent) = d.parent_instance_id.as_deref() {
            let _ = writeln!(out, "  topology   parent {parent}");
        }
        if d.suspend_allowed == Some(true) {
            let _ = writeln!(
                out,
                "  suspend    this port's hub chain may be powered down by Windows"
            );
        }
        let _ = writeln!(out, "  verdict    {}", v.summary);
        if !v.remedy.is_empty() {
            let _ = writeln!(out, "  remedy     {}", v.remedy);
        }
        out.push('\n');
    }

    if !problems.is_empty() {
        out.push_str("unassociated USB problem device(s)\n");
        for p in problems {
            let name = p.friendly_name.as_deref().unwrap_or("Unknown USB device");
            let location = p.location.as_deref().unwrap_or("location unavailable");
            let _ = writeln!(out, "  {name}");
            let _ = writeln!(
                out,
                "    problem {} at {location}  instance {}",
                p.problem_code, p.instance_id
            );
        }
        out.push_str(
            "  NOTE: these are not attributed to any port above. A failing device on one\n\
             \x20       port says nothing about a stale record on another — check the\n\
             \x20       location before assuming they are the same board.\n",
        );
    }

    out
}

/// Does this port fall within the requested scope?
///
/// `--port` matches the COM name exactly (case-insensitively). `--hub`
/// matches any USB **ancestor**, by substring, so a partial instance ID or a
/// bare VID:PID fragment works — you rarely have the full instance string to
/// hand when you are looking at a hub in Device Manager. Neither given means
/// every port.
pub fn port_in_scope(
    port_name: &str,
    ancestors: &[String],
    only_port: Option<&str>,
    only_hub: Option<&str>,
) -> bool {
    if let Some(want) = only_port {
        return port_name.eq_ignore_ascii_case(want);
    }
    if let Some(hub) = only_hub {
        let hub = hub.to_ascii_uppercase();
        return ancestors
            .iter()
            .any(|a| a.to_ascii_uppercase().contains(&hub));
    }
    true
}

/// Machine-readable report. Mirrors the human output's structure so a script
/// and a person are reading the same model.
#[derive(Debug, serde::Serialize)]
pub struct JsonReport {
    pub ports: Vec<JsonPort>,
    /// Present USB devices with a fault that could not be tied to any port.
    /// A separate list, not a field on a port, because attributing one to a
    /// nearby board is the misdiagnosis this command exists to prevent.
    pub unassociated_problem_devices: Vec<JsonProblemDevice>,
    /// `null` when the host cannot say — never silently `false`.
    pub selective_suspend_enabled: Option<bool>,
}

#[derive(Debug, serde::Serialize)]
pub struct JsonPort {
    #[serde(flatten)]
    pub diagnosis: PortDiagnosis,
    pub verdict: Verdict,
}

#[derive(Debug, serde::Serialize)]
pub struct JsonProblemDevice {
    pub instance_id: String,
    pub problem_code: u32,
    pub problem_meaning: Option<&'static str>,
    pub friendly_name: Option<String>,
    pub location: Option<String>,
}

pub fn build_json_report(
    diagnoses: &[PortDiagnosis],
    problems: &[UsbProblemDevice],
    selective_suspend_enabled: Option<bool>,
) -> JsonReport {
    JsonReport {
        ports: diagnoses
            .iter()
            .map(|d| JsonPort {
                diagnosis: d.clone(),
                verdict: verdict(d),
            })
            .collect(),
        unassociated_problem_devices: problems
            .iter()
            .map(|p| JsonProblemDevice {
                instance_id: p.instance_id.clone(),
                problem_code: p.problem_code,
                problem_meaning: problem_code_meaning(p.problem_code),
                friendly_name: p.friendly_name.clone(),
                location: p.location.clone(),
            })
            .collect(),
        selective_suspend_enabled,
    }
}

/// `fbuild port doctor` entry point.
pub fn run(only_port: Option<&str>, only_hub: Option<&str>, json: bool) -> Result<()> {
    let power_rows = query_device_power_rows();
    let ports = fbuild_serial::ports::available_ports()
        .map_err(|e| FbuildError::SerialError(format!("serial port enumeration failed: {e}")))?;
    let mut diagnoses: Vec<_> = ports
        .iter()
        // Explicit match rather than `Option::is_none_or`: that is stable only
        // since 1.82 and `.clippy.toml` pins msrv = 1.75.
        .filter(|p| {
            port_in_scope(
                &p.info.port_name,
                &p.ancestor_instance_ids,
                only_port,
                only_hub,
            )
        })
        .map(|p| diagnose(p, &power_rows))
        .collect();
    if diagnoses.is_empty() {
        // Being explicit beats silence: a selector that matches nothing is
        // itself a finding, not an empty report.
        if let Some(want) = only_port {
            return Err(FbuildError::SerialError(format!(
                "no serial port named {want}; run `fbuild port scan` to list what the host sees"
            )));
        }
        if let Some(hub) = only_hub {
            // Distinguish "no match" from "we have no topology to match
            // against". Off Windows the ancestor chain is always empty, so
            // blaming the hub string would send the reader looking for a
            // typo that is not there.
            if ports.iter().all(|p| p.ancestor_instance_ids.is_empty()) {
                return Err(FbuildError::SerialError(
                    "--hub needs USB topology, which this host does not expose \
                     (normal off Windows); re-run without --hub"
                        .to_string(),
                ));
            }
            return Err(FbuildError::SerialError(format!(
                "no port sits behind a USB ancestor matching {hub}; \
                 run `fbuild port doctor` to see each port's topology"
            )));
        }
    }
    // Only for a targeted query: this costs ~0.7s per device and does not
    // batch, so the all-ports listing deliberately goes without.
    if only_port.is_some() {
        for d in diagnoses.iter_mut() {
            if let Some(instance) = d.instance_id.as_deref() {
                d.last_seen_secs_ago = query_last_seen_secs(instance).map(|arrived| {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(arrived);
                    now - arrived
                });
            }
        }
    }
    let problems = fbuild_serial::ports::present_usb_problem_devices();
    let suspend = query_selective_suspend();
    if json {
        let report = build_json_report(&diagnoses, &problems, suspend);
        let text = serde_json::to_string_pretty(&report)
            .map_err(|e| FbuildError::SerialError(format!("json serialization failed: {e}")))?;
        crate::output::result(&text);
        return Ok(());
    }
    let mut report = render_report(&diagnoses, &problems);
    report.push_str(&render_suspend_section(suspend));
    crate::output::result(report.trim_end_matches('\n'));
    Ok(())
}

/// Read the active power plan's USB selective-suspend setting.
///
/// Read-only and best-effort — `doctor` must never fail because a diagnostic
/// probe did. `None` (its value off Windows, or when powercfg is unavailable
/// or its output unrecognised) simply omits the section rather than asserting
/// the host is fine.
/// Read the per-device "allow the computer to turn off this device" flags.
///
/// One WMI call for the whole report (~0.5s), not one per port. Deliberately
/// **not** wired into `port scan`: that is a hot path used by deploy, and a
/// diagnostic is the right place to pay for this.
///
/// Read-only and best-effort; an empty result simply omits the per-port line.
fn query_device_power_rows() -> Vec<(String, bool)> {
    if !cfg!(windows) {
        return Vec::new();
    }
    let script = "Get-CimInstance -Namespace root\\wmi -ClassName MSPower_DeviceEnable \
                  -ErrorAction SilentlyContinue | ForEach-Object { \
                  Write-Output (\"{0}|{1}\" -f $_.InstanceName, $_.Enable) }";
    let Ok(out) = fbuild_core::subprocess::run_command_blocking(
        &[
            "powershell",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            script,
        ],
        None,
        None,
        Some(std::time::Duration::from_secs(20)),
    ) else {
        return Vec::new();
    };
    parse_device_power_rows(&out.stdout)
}

/// Seconds since this devnode was last seen on the bus.
///
/// Only ever called for a **single** port. `Get-PnpDeviceProperty` costs
/// ~0.7s per device and does not batch, so asking for every port would add
/// ~12s to the listing (measured, 17 ports). Restricting it to `--port` puts
/// the answer exactly where the question is asked — "is this board broken or
/// just unplugged?" — without taxing the general report.
///
/// This is the one path that works: `SetupDiGetDevicePropertyW` returns
/// nothing for a phantom devnode, `CM_Get_DevNode_PropertyW` returns
/// `CR_NO_SUCH_VALUE` even after `CM_Locate_DevNodeW(..., PHANTOM)`, and the
/// registry copy needs elevation. The CIM provider behind
/// `Get-PnpDeviceProperty` answers unelevated for phantoms.
fn query_last_seen_secs(instance_id: &str) -> Option<i64> {
    if !cfg!(windows) {
        return None;
    }
    let script = format!(
        "$d = Get-PnpDeviceProperty -InstanceId '{}' \
         -KeyName 'DEVPKEY_Device_LastArrivalDate' -ErrorAction SilentlyContinue; \
         if ($d -and $d.Data) {{ Write-Output (\"x|{{0}}\" -f \
         ([DateTimeOffset]$d.Data).ToUnixTimeSeconds()) }}",
        instance_id.replace('\'', "''")
    );
    let out = fbuild_core::subprocess::run_command_blocking(
        &[
            "powershell",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &script,
        ],
        None,
        None,
        Some(std::time::Duration::from_secs(30)),
    )
    .ok()?;
    parse_last_seen_rows(&out.stdout)
}

pub(crate) fn query_selective_suspend() -> Option<bool> {
    if !cfg!(windows) {
        return None;
    }
    let out = fbuild_core::subprocess::run_command_blocking(
        &[
            "powercfg",
            "/q",
            "SCHEME_CURRENT",
            USB_SUBGROUP_GUID,
            SELECTIVE_SUSPEND_GUID,
        ],
        None,
        None,
        Some(std::time::Duration::from_secs(15)),
    )
    .ok()?;
    parse_selective_suspend(&out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diag(presence: Option<bool>, problem: Option<u32>) -> PortDiagnosis {
        PortDiagnosis {
            port: "COM17".to_string(),
            presence,
            health: "phantom".to_string(),
            problem_code: problem,
            instance_id: Some(r"USB\VID_2E8A&PID_F00F\X".to_string()),
            parent_instance_id: None,
            suspend_allowed: None,
            last_seen_secs_ago: None,
        }
    }

    /// The headline: an absent board must be called out as absent, and the
    /// remedy must be "plug it in" — never a recovery procedure.
    #[test]
    fn absent_board_is_not_reported_as_a_fault() {
        let v = verdict(&diag(Some(false), None));
        assert!(v.summary.contains("not attached"), "got: {}", v.summary);
        assert!(v.summary.contains("not a fault"), "got: {}", v.summary);
        assert!(v.remedy.contains("plug the board in"), "got: {}", v.remedy);
    }

    /// An attached-but-failing board is the opposite case and must not be
    /// described as absent.
    #[test]
    fn attached_but_failing_board_is_distinguished_from_absent() {
        let v = verdict(&diag(Some(true), Some(43)));
        assert!(
            v.summary.contains("attached but failed"),
            "got: {}",
            v.summary
        );
        assert!(!v.summary.contains("not attached"), "got: {}", v.summary);
    }

    /// The code-43 remedy must steer away from bus resets, which provably do
    /// not recover a descriptor-failed device, and toward the BOOTSEL test
    /// that separates firmware faults from hardware faults.
    #[test]
    fn code_43_remedy_rejects_bus_reset_and_names_the_bootsel_test() {
        let v = verdict(&diag(Some(true), Some(43)));
        assert!(v.remedy.contains("not a VBUS cycle"), "got: {}", v.remedy);
        assert!(v.remedy.contains("BOOTSEL"), "got: {}", v.remedy);
    }

    #[test]
    fn healthy_port_needs_no_remedy() {
        let v = verdict(&diag(Some(true), None));
        assert!(!v.needs_hands);
        assert!(v.remedy.is_empty());
    }

    /// Off Windows we cannot see presence and must not claim absence.
    #[test]
    fn unknown_presence_is_not_reported_as_absent() {
        let v = verdict(&diag(None, None));
        assert!(!v.summary.contains("not attached"), "got: {}", v.summary);
        assert!(!v.needs_hands);
    }

    #[test]
    fn unknown_problem_codes_render_the_number_rather_than_guessing() {
        assert!(problem_code_meaning(4242).is_none());
        let v = verdict(&diag(Some(true), Some(4242)));
        assert!(v.summary.contains("4242"), "got: {}", v.summary);
    }

    /// Unassociated failing devices get their own section with an explicit
    /// warning, because attributing one to a nearby port is the exact
    /// misdiagnosis this command exists to prevent.
    #[test]
    fn unassociated_problem_devices_are_not_attributed_to_a_port() {
        let problems = vec![UsbProblemDevice {
            instance_id: r"USB\VID_0000&PID_0002\6&3AF0F9CE&0&14".to_string(),
            problem_code: 43,
            friendly_name: Some("Unknown USB Device (Device Descriptor Request Failed)".into()),
            location: Some("Port_#0014.Hub_#0001".into()),
            behind_external_hub: Some(false),
            device_class: None,
            parent_instance_id: None,
        }];
        let out = render_report(&[diag(Some(false), None)], &problems);
        assert!(
            out.contains("unassociated USB problem device(s)"),
            "got: {out}"
        );
        assert!(
            out.contains("not attributed to any port above"),
            "got: {out}"
        );
        assert!(out.contains("Port_#0014.Hub_#0001"), "got: {out}");
    }

    #[test]
    fn empty_port_list_renders_a_message_rather_than_nothing() {
        assert!(render_report(&[], &[]).contains("no serial ports visible"));
    }

    const POWERCFG_ENABLED: &str = "\
Power Scheme GUID: 381b4222-f694-41f0-9685-ff5bb260df2e  (Balanced)
    Power Setting GUID: 48e6b7a6-50f5-4782-a5d4-53bb8f07e226  (USB selective suspend setting)
      Current AC Power Setting Index: 0x00000001
      Current DC Power Setting Index: 0x00000001
";

    #[test]
    fn selective_suspend_parsed_from_powercfg() {
        assert_eq!(parse_selective_suspend(POWERCFG_ENABLED), Some(true));
        let disabled = POWERCFG_ENABLED.replace("0x00000001", "0x00000000");
        assert_eq!(parse_selective_suspend(&disabled), Some(false));
    }

    /// Unparseable output must be "no opinion", never a false "disabled" —
    /// otherwise the report would quietly clear a host nobody checked.
    #[test]
    fn unparseable_powercfg_output_is_not_reported_as_disabled() {
        assert_eq!(parse_selective_suspend(""), None);
        assert_eq!(parse_selective_suspend("garbage"), None);
        assert_eq!(
            parse_selective_suspend("Current AC Power Setting Index: zzz"),
            None
        );
    }

    #[test]
    fn suspend_section_names_the_phantom_symptom_and_the_remedy() {
        let out = render_suspend_section(Some(true));
        assert!(out.contains("health=phantom"), "got: {out}");
        assert!(out.contains("powercfg -setacvalueindex"), "got: {out}");
    }

    #[test]
    fn suspend_section_is_silent_when_unknown() {
        assert!(render_suspend_section(None).is_empty());
        assert!(render_suspend_section(Some(false)).contains("disabled"));
    }

    /// The JSON must keep unassociated problem devices in their own list
    /// rather than hanging them off a port — the same separation the text
    /// report enforces, for the same reason.
    #[test]
    fn json_keeps_problem_devices_separate_from_ports() {
        let problems = vec![UsbProblemDevice {
            instance_id: r"USB\VID_0000&PID_0002\X".to_string(),
            problem_code: 43,
            friendly_name: None,
            location: Some("Port_#0014.Hub_#0001".into()),
            behind_external_hub: Some(false),
            device_class: None,
            parent_instance_id: None,
        }];
        let report = build_json_report(&[diag(Some(false), None)], &problems, Some(true));
        assert_eq!(report.ports.len(), 1);
        assert_eq!(report.unassociated_problem_devices.len(), 1);
        let text = serde_json::to_string(&report).unwrap();
        assert!(text.contains("unassociated_problem_devices"), "got: {text}");
        // problem codes are translated for machine consumers too
        assert!(
            text.contains("device descriptor request failed"),
            "got: {text}"
        );
    }

    /// `null`, not `false`: a consumer must be able to tell "not checked"
    /// from "checked and fine".
    #[test]
    fn json_suspend_unknown_serializes_as_null() {
        let report = build_json_report(&[], &[], None);
        let text = serde_json::to_string(&report).unwrap();
        assert!(
            text.contains("\"selective_suspend_enabled\":null"),
            "got: {text}"
        );
    }

    #[test]
    fn json_carries_the_verdict_alongside_each_port() {
        let report = build_json_report(&[diag(Some(false), None)], &[], Some(false));
        let text = serde_json::to_string(&report).unwrap();
        assert!(text.contains("not attached"), "got: {text}");
        assert!(text.contains("\"presence\":false"), "got: {text}");
    }

    #[test]
    fn device_power_rows_parsed_and_suffix_trimmed() {
        let rows = parse_device_power_rows(
            "USB\\VID_05E3&PID_0610\\7&3AFC677D&0&1_0|True\nUSB\\ROOT_HUB30\\5&4087D53&0&0_0|False\n",
        );
        assert_eq!(rows.len(), 2);
        // the WMI `_0` suffix is not part of the PnP instance ID
        assert!(rows[0].0.ends_with("&0&1"), "got: {}", rows[0].0);
        assert!(rows[0].1);
        assert!(!rows[1].1);
    }

    /// The suspendable node is typically a *grandparent* hub, not the
    /// immediate parent. Matching only one hop reported `None` for every real
    /// port on this bench — caught by running it, not by the tests, because
    /// the fixtures matched by construction.
    #[test]
    fn suspend_walks_the_whole_ancestor_chain() {
        let rows = parse_device_power_rows("USB\\VID_05E3&PID_0610\\7&3AFC677D&0&1_0|True\n");
        let chain = vec![
            // composite device — the immediate parent, carries no power policy
            r"USB\VID_303A&PID_1001\8C:BF:EA:CF:87:B4".to_string(),
            // the hub, two hops up, lowercased to pin case-insensitivity
            r"USB\VID_05E3&PID_0610\7&3afc677d&0&1".to_string(),
        ];
        assert_eq!(suspend_for_ancestors(&rows, &chain), Some(true));
    }

    /// Unix seconds, not a rendered date: `Get-PnpDeviceProperty` formats
    /// dates per-locale, so parsing text would break on a non-English host.
    #[test]
    fn last_seen_parsed_from_unix_seconds() {
        assert_eq!(parse_last_seen_rows("x|1785648479\n"), Some(1785648479));
        assert_eq!(parse_last_seen_rows(""), None);
        assert_eq!(parse_last_seen_rows("x|8/1/2026 10:27:59 PM"), None);
        assert_eq!(parse_last_seen_rows("garbage"), None);
    }

    #[test]
    fn scope_defaults_to_every_port() {
        assert!(port_in_scope("COM9", &[], None, None));
    }

    #[test]
    fn scope_port_matches_case_insensitively_and_exactly() {
        assert!(port_in_scope("COM9", &[], Some("com9"), None));
        assert!(!port_in_scope("COM9", &[], Some("COM19"), None));
        // must not match on prefix — COM1 is not COM19
        assert!(!port_in_scope("COM19", &[], Some("COM1"), None));
    }

    /// `--hub` matches any ancestor by substring: you rarely have the full
    /// instance ID to hand when looking at a hub in Device Manager.
    #[test]
    fn scope_hub_matches_any_ancestor_by_substring() {
        let chain = vec![
            r"USB\VID_303A&PID_1001\8C:BF:EA:CF:87:B4".to_string(),
            r"USB\VID_05E3&PID_0610\7&3afc677d&0&1".to_string(),
        ];
        assert!(port_in_scope("COM9", &chain, None, Some("VID_05E3")));
        assert!(port_in_scope("COM9", &chain, None, Some("vid_05e3")));
        assert!(!port_in_scope("COM9", &chain, None, Some("VID_DEAD")));
        assert!(!port_in_scope("COM9", &[], None, Some("VID_05E3")));
    }

    /// An explicit port wins over a hub filter, so the narrower request is
    /// never silently widened.
    #[test]
    fn scope_port_takes_precedence_over_hub() {
        let chain = vec![r"USB\VID_05E3&PID_0610\X".to_string()];
        assert!(!port_in_scope(
            "COM9",
            &chain,
            Some("COM17"),
            Some("VID_05E3")
        ));
    }

    /// An unrecognised `Enable` value is no data, not "cannot be powered
    /// off" — treating it as false would quietly clear a device nobody
    /// checked.
    #[test]
    fn unrecognised_enable_value_is_dropped_not_treated_as_false() {
        let rows = parse_device_power_rows("A|True\nB|False\nC|\nD|maybe\nE|(null)\n");
        let names: Vec<&str> = rows.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["A", "B"], "only recognised values survive");
    }

    /// `trim_end_matches` strips repeatedly; an instance legitimately ending
    /// `_0_0` would lose both suffixes and stop matching its devnode.
    #[test]
    fn only_one_wmi_suffix_is_stripped() {
        let rows = parse_device_power_rows("USB\\X_0_0|True\n");
        assert_eq!(rows[0].0, "USB\\X_0");
    }

    /// The negative branch must not render as "last seen in the future ago".
    #[test]
    fn future_timestamp_renders_as_a_sentence_not_nonsense() {
        let s = format_last_seen(-5);
        assert!(!s.contains("ago"), "got: {s}");
        assert!(s.contains("future"), "got: {s}");
        assert_eq!(format_last_seen(90), "1m ago");
    }

    #[test]
    fn age_renders_coarsely_enough_to_read_at_a_glance() {
        assert_eq!(format_age(45), "45s");
        assert_eq!(format_age(90), "1m");
        assert_eq!(format_age(7_200), "2h");
        assert_eq!(format_age(5 * 86_400), "5d");
        // Negative clamps here; the readable phrasing for a future timestamp
        // lives in format_last_seen, so "in the future ago" is impossible.
        assert_eq!(format_age(-5), "0s");
    }

    #[test]
    fn suspend_reports_false_only_when_a_row_actually_matched() {
        let rows = parse_device_power_rows("USB\\VID_05E3&PID_0610\\7&3AFC677D&0&1_0|False\n");
        let chain = vec![r"USB\VID_05E3&PID_0610\7&3AFC677D&0&1".to_string()];
        assert_eq!(suspend_for_ancestors(&rows, &chain), Some(false));
        // Nothing matched: unknown, not "fine". A false clear is worse than
        // saying nothing.
        let other = vec![r"USB\VID_DEAD&PID_BEEF\X".to_string()];
        assert_eq!(suspend_for_ancestors(&rows, &other), None);
        assert_eq!(suspend_for_ancestors(&rows, &[]), None);
        assert_eq!(suspend_for_ancestors(&[], &chain), None);
    }
}
