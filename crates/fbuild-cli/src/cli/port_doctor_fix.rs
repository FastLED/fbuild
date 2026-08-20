//! `fbuild port doctor --fix` — the one mutating path.
//!
//! Split from `port_doctor` so the read-only diagnosis and the host-changing
//! remedy stay separate concerns (and to keep both files under this repo's
//! 1000-LOC cap).

use fbuild_core::{FbuildError, Result};

use super::port_doctor::{SELECTIVE_SUSPEND_GUID, USB_SUBGROUP_GUID, query_selective_suspend};

/// The exact `powercfg` argv `--fix` would run, AC and DC.
///
/// Pure so the change set can be shown by `--dry-run` and asserted in tests
/// without touching the host. Both indices matter: disabling only the AC side
/// leaves a laptop suspending ports the moment it is unplugged.
pub fn suspend_fix_commands() -> Vec<Vec<String>> {
    ["-setacvalueindex", "-setdcvalueindex"]
        .iter()
        .map(|verb| {
            vec![
                "powercfg".to_string(),
                (*verb).to_string(),
                "SCHEME_CURRENT".to_string(),
                USB_SUBGROUP_GUID.to_string(),
                SELECTIVE_SUSPEND_GUID.to_string(),
                "0".to_string(),
            ]
        })
        .collect()
}

/// Human-readable plan for `--fix`, used by `--dry-run` and before elevating.
pub fn render_fix_plan(commands: &[Vec<String>], already_disabled: bool) -> String {
    if already_disabled {
        // Idempotent: nothing to do, and in particular no UAC prompt for a
        // change that would be a no-op.
        return "nothing to do — USB selective suspend is already disabled\n".to_string();
    }
    let mut out = String::from("would run, elevated:\n");
    for cmd in commands {
        out.push_str("  ");
        out.push_str(&cmd.join(" "));
        out.push('\n');
    }
    out.push_str(
        "this changes a host-wide power setting; revert with the same commands and a \
         trailing 1 instead of 0\n",
    );
    out
}

/// Apply the safe subset of remedies: disable USB selective suspend.
///
/// Deliberately narrow. It does **not** disable/enable devnodes or restart
/// hubs: a bus reset is not a VBUS cycle and does not recover a
/// descriptor-failed device, while a root-hub restart disrupts every other
/// device on that hub. A fix that looks helpful and is not is worse than none.
pub fn run_fix(dry_run: bool, assume_yes: bool, no_elevate: bool) -> Result<()> {
    let current = query_selective_suspend();
    let already_disabled = current == Some(false);
    let commands = suspend_fix_commands();
    crate::output::result(render_fix_plan(&commands, already_disabled).trim_end_matches('\n'));

    if already_disabled || dry_run {
        return Ok(());
    }
    if !fbuild_core::platform::host::is_windows() {
        crate::output::result("not applicable on this platform");
        return Ok(());
    }
    if no_elevate {
        return Err(FbuildError::SerialError(
            "--no-elevate was passed but this change needs administrator rights; \
             re-run without it, or run the commands above from an elevated shell"
                .to_string(),
        ));
    }
    if !assume_yes {
        // A host-wide power-policy change should never be a side effect of a
        // diagnostic. Require the caller to say so.
        return Err(FbuildError::SerialError(
            "this changes a host-wide power setting; re-run with --yes to apply, \
             or --dry-run to see the plan only"
                .to_string(),
        ));
    }

    // One elevation for the whole change set, not one per command.
    let joined = commands
        .iter()
        .map(|c| c.join(" "))
        .collect::<Vec<_>>()
        .join("; ");
    let script =
        format!("Start-Process -Verb RunAs -Wait -FilePath cmd -ArgumentList '/c {joined}'");
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
        Some(std::time::Duration::from_secs(120)),
    )
    .map_err(|e| FbuildError::SerialError(format!("elevation failed: {e}")))?;
    if !out.success() {
        return Err(FbuildError::SerialError(format!(
            "elevated powercfg failed: {}",
            out.stderr.trim()
        )));
    }
    crate::output::result("USB selective suspend disabled; re-run `fbuild port doctor` to confirm");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both indices matter: disabling only AC leaves a laptop suspending
    /// ports the moment it is unplugged.
    #[test]
    fn fix_covers_both_ac_and_dc() {
        let cmds = suspend_fix_commands();
        assert_eq!(cmds.len(), 2);
        assert!(cmds.iter().any(|c| c.contains(&"-setacvalueindex".into())));
        assert!(cmds.iter().any(|c| c.contains(&"-setdcvalueindex".into())));
        for c in &cmds {
            assert_eq!(c.last().unwrap(), "0", "must disable, not enable");
            assert!(c.contains(&USB_SUBGROUP_GUID.to_string()));
            assert!(c.contains(&SELECTIVE_SUSPEND_GUID.to_string()));
        }
    }

    /// Idempotence: an already-disabled host must produce no plan, so `--fix`
    /// never raises a UAC prompt for a no-op.
    #[test]
    fn fix_plan_is_empty_when_already_disabled() {
        let plan = render_fix_plan(&suspend_fix_commands(), true);
        assert!(plan.contains("nothing to do"), "got: {plan}");
        assert!(!plan.contains("would run"), "got: {plan}");
    }

    /// A host-wide change must show exactly what it will run, and how to undo it.
    #[test]
    fn fix_plan_shows_commands_and_how_to_revert() {
        let plan = render_fix_plan(&suspend_fix_commands(), false);
        assert!(plan.contains("would run, elevated"), "got: {plan}");
        assert!(plan.contains("-setacvalueindex"), "got: {plan}");
        assert!(plan.contains("revert"), "got: {plan}");
    }
}
