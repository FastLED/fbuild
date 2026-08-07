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

use fbuild_serial::ports::{DetectedPort, UsbProblemDevice};

/// One port's diagnosis, decoupled from rendering so the verdict logic is
/// testable without a live device tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortDiagnosis {
    pub port: String,
    /// `Some(true)` attached, `Some(false)` a stale record, `None` unknowable.
    pub presence: Option<bool>,
    pub health: String,
    pub problem_code: Option<u32>,
    pub instance_id: Option<String>,
    pub parent_instance_id: Option<String>,
}

/// What the diagnosis means and what to do about it.
#[derive(Clone, Debug, PartialEq, Eq)]
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

pub fn diagnose(port: &DetectedPort) -> PortDiagnosis {
    PortDiagnosis {
        port: port.info.port_name.clone(),
        presence: port.health.is_present(),
        health: port.health.label().to_string(),
        problem_code: port.health.problem_code(),
        instance_id: port.instance_id.clone(),
        parent_instance_id: port.parent_instance_id.clone(),
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
}
