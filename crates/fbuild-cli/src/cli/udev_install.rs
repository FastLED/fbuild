//! Install the udev rules from [`super::udev`], asking for root with whatever
//! prompt the host has (FastLED/fbuild#1621).
//!
//! Runs in the CLI, never the daemon: only the CLI has the user's tty and
//! display, which the root prompt needs. The decisions (where to install,
//! how to elevate, whether to ask at all) are pure functions over
//! [`HostEnv`] so they are testable without root; [`install`] is the only
//! part that touches the host.

use std::path::Path;

use fbuild_core::path::NormalizedPath;

use fbuild_core::{FbuildError, Result};

use super::udev::{LEGACY_UDEV_RULES_FILENAME, UDEV_RULES_FILENAME, render_udev_rules};

/// Persistent rules directory on ordinary distros.
pub const ETC_RULES_DIR: &str = "/etc/udev/rules.d";
/// Runtime rules directory udevd also searches. On NixOS `/etc/udev/rules.d`
/// is a read-only store symlink, so this is the only writable choice there;
/// it lasts until reboot.
pub const RUN_RULES_DIR: &str = "/run/udev/rules.d";

/// Opt-out for the automatic prompt before deploy/monitor/debug.
pub const NO_ELEVATE_ENV: &str = "FBUILD_NO_ELEVATE";

/// What the host offers for getting root, gathered once by [`HostEnv::probe`].
#[derive(Debug, Clone, Default)]
pub struct HostEnv {
    pub is_root: bool,
    /// `DISPLAY` or `WAYLAND_DISPLAY` is set, so a polkit agent can show a dialog.
    pub has_display: bool,
    pub stdin_is_tty: bool,
    /// `CI` is set: never prompt.
    pub ci: bool,
    pub no_elevate: bool,
    pub nixos: bool,
    pub pkexec: Option<NormalizedPath>,
    pub sudo: Option<NormalizedPath>,
    /// `SUDO_ASKPASS`, or a GUI askpass helper found on PATH.
    pub askpass: Option<NormalizedPath>,
}

impl HostEnv {
    pub fn probe() -> Self {
        let var = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
        let askpass = std::env::var_os("SUDO_ASKPASS")
            .map(NormalizedPath::new)
            .or_else(|| {
                ["ssh-askpass", "ksshaskpass", "lxqt-openssh-askpass"]
                    .iter()
                    .find_map(|n| which(n))
            });
        Self {
            is_root: is_root(),
            has_display: var("DISPLAY") || var("WAYLAND_DISPLAY"),
            stdin_is_tty: std::io::IsTerminal::is_terminal(&std::io::stdin()),
            ci: var("CI"),
            no_elevate: std::env::var(NO_ELEVATE_ENV).is_ok_and(|v| v == "1"),
            nixos: Path::new("/etc/NIXOS").exists(),
            pkexec: which("pkexec"),
            sudo: which("sudo"),
            askpass,
        }
    }
}

/// How the privileged step is run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Elevation {
    /// Already root.
    Direct,
    /// polkit: the desktop agent shows a dialog, or pkexec's text agent asks
    /// on the tty when no GUI agent is running.
    Pkexec(NormalizedPath),
    /// `sudo` prompting on the terminal.
    Sudo(NormalizedPath),
    /// `sudo -A` with a GUI askpass helper, for desktops without polkit.
    SudoAskpass {
        sudo: NormalizedPath,
        askpass: NormalizedPath,
    },
}

/// Pick the first elevation method this host can actually prompt with.
pub fn choose_elevation(env: &HostEnv) -> Option<Elevation> {
    if env.is_root {
        return Some(Elevation::Direct);
    }
    if let Some(pkexec) = &env.pkexec {
        if env.has_display || env.stdin_is_tty {
            return Some(Elevation::Pkexec(pkexec.clone()));
        }
    }
    if let Some(sudo) = &env.sudo {
        if env.stdin_is_tty {
            return Some(Elevation::Sudo(sudo.clone()));
        }
        if let (Some(askpass), true) = (&env.askpass, env.has_display) {
            return Some(Elevation::SudoAskpass {
                sudo: sudo.clone(),
                askpass: askpass.clone(),
            });
        }
    }
    None
}

/// Whether deploy/monitor/debug may raise the prompt on their own.
pub fn may_prompt_automatically(env: &HostEnv) -> bool {
    !env.ci && !env.no_elevate && (env.is_root || env.has_display || env.stdin_is_tty)
}

/// Directory the rules go into on this host.
pub fn rules_dir(env: &HostEnv) -> &'static str {
    if env.nixos {
        RUN_RULES_DIR
    } else {
        ETC_RULES_DIR
    }
}

/// Name of the root helper. pkexec's dialog shows the program it is about to
/// run, so this name -- not a `sh -c` one-liner -- is what the user reads.
pub const HELPER_NAME: &str = "fbuild-install-usb-rules";

/// The root helper. Its only inputs are positional paths fbuild generated --
/// `$1` destination, `$2` rendered rules, `$3` legacy file to remove, `$4`
/// absolute udevadm -- so no user text reaches a root shell. The destination
/// comes first so it is the part of the command a truncated dialog still shows.
pub const HELPER_SCRIPT: &str = "#!/bin/sh\n\
# fbuild (FastLED/fbuild#1621): install the USB udev rules and reload udev.\n\
set -e\n\
install -D -m 0644 \"$2\" \"$1\"\n\
rm -f \"$3\"\n\
\"$4\" control --reload-rules\n\
\"$4\" trigger --action=add --subsystem-match=usb --subsystem-match=tty --subsystem-match=hidraw\n";

/// Title of fbuild's own explanation dialog.
pub const DIALOG_TITLE: &str = "fbuild: install USB device rules";

/// argv that runs the helper under `elevation`.
pub fn elevated_argv(
    elevation: &Elevation,
    helper: &Path,
    dest: &Path,
    rules: &Path,
    legacy: &Path,
    udevadm: &Path,
) -> Vec<String> {
    // `display_slash` is identity on Unix, where this argv is executed; it
    // keeps the rendering host-independent so the argv tests hold on Windows
    // too, where `NormalizedPath` would otherwise print `\usr\bin\pkexec`.
    let mut argv: Vec<String> = match elevation {
        Elevation::Direct => vec![],
        Elevation::Pkexec(p) => vec![p.display_slash()],
        Elevation::Sudo(s) => vec![s.display_slash()],
        Elevation::SudoAskpass { sudo, .. } => vec![sudo.display_slash(), "-A".into()],
    };
    argv.extend(
        [helper, dest, rules, legacy, udevadm]
            .iter()
            .map(|p| p.display().to_string()),
    );
    argv
}

/// The command a user can run themselves instead of granting fbuild root.
pub fn manual_command(dest: &Path) -> String {
    format!(
        "fbuild port udev | sudo tee {} >/dev/null && sudo udevadm control --reload-rules \
         && sudo udevadm trigger",
        dest.display()
    )
}

/// Why fbuild is asking, shown before the system password prompt.
pub fn explanation(dest: &Path, nixos: bool) -> String {
    let lifetime = if nixos {
        "This is a one-time setup. On NixOS it lasts until reboot; fbuild then prints the \
         configuration.nix lines that make it permanent."
    } else {
        "This is a one-time setup for this computer."
    };
    format!(
        "fbuild needs to install USB device rules (udev) so your user account can open the \
         attached development boards -- their serial ports and debug probes such as the \
         LPC-Link2 -- without root.\n\n\
         {lifetime} It writes {} and reloads udev; nothing else is changed.\n\n\
         Your system will ask for your password next.\n\n\
         To do it yourself instead, run:\n  {}",
        dest.display(),
        manual_command(dest)
    )
}

/// Show [`explanation`] and ask to continue: kdialog on KDE, zenity
/// elsewhere, terminal text otherwise (the password prompt that follows is
/// then the consent). Returns false when the user chose Skip.
fn confirm(env: &HostEnv, text: &str) -> bool {
    if env.has_display {
        let dialog: Option<Vec<String>> = if let Some(k) = which("kdialog") {
            Some(vec![
                k.display().to_string(),
                "--title".into(),
                DIALOG_TITLE.into(),
                "--yes-label".into(),
                "Install".into(),
                "--no-label".into(),
                "Skip".into(),
                "--yesno".into(),
                text.into(),
            ])
        } else {
            which("zenity").map(|z| {
                vec![
                    z.display().to_string(),
                    "--question".into(),
                    "--no-markup".into(),
                    "--width=560".into(),
                    format!("--title={DIALOG_TITLE}"),
                    "--ok-label=Install".into(),
                    "--cancel-label=Skip".into(),
                    format!("--text={text}"),
                ]
            })
        };
        if let Some(argv) = dialog {
            let argv_ref: Vec<&str> = argv.iter().map(String::as_str).collect();
            if let Ok(out) = fbuild_core::subprocess::run_command_blocking(
                &argv_ref,
                None,
                None,
                Some(std::time::Duration::from_secs(600)),
            ) {
                return out.success();
            }
        }
    }
    crate::output::diagnostic(text);
    true
}

/// Result of [`install`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallOutcome {
    Installed,
    AlreadyCurrent,
    /// The user chose Skip in fbuild's explanation dialog.
    Skipped,
}

/// Render the rules for the current registry, or explain why not.
pub fn render_current_rules() -> Result<String> {
    let vids = fbuild_core::usb::online_vendor_vids();
    render_udev_rules(&vids, super::udev::host_default_group()).ok_or_else(|| {
        FbuildError::Other(
            "USB vendor registry is empty — cannot generate udev rules. Run \
             `fbuild port scan` once with network access, then retry."
                .to_string(),
        )
    })
}

/// Whether `dir` already holds exactly these rules.
pub fn rules_current(dir: &Path, rules: &str) -> bool {
    std::fs::read_to_string(dir.join(UDEV_RULES_FILENAME)).is_ok_and(|have| have == rules)
}

/// Explain, ask, then install `rules` through the host's root prompt.
pub fn install(rules: &str, env: &HostEnv) -> Result<InstallOutcome> {
    let dir = Path::new(rules_dir(env));
    let dest = dir.join(UDEV_RULES_FILENAME);
    if rules_current(dir, rules) {
        return Ok(InstallOutcome::AlreadyCurrent);
    }
    let elevation = choose_elevation(env).ok_or_else(|| {
        FbuildError::Other(format!(
            "cannot ask for root here (no pkexec with a display, no sudo on a terminal). \
             Install the USB rules yourself:\n  {}",
            manual_command(&dest)
        ))
    })?;
    if elevation != Elevation::Direct && !confirm(env, &explanation(&dest, env.nixos)) {
        crate::output::diagnostic(format!(
            "skipped installing USB rules; to do it yourself run:\n  {}",
            manual_command(&dest)
        ));
        return Ok(InstallOutcome::Skipped);
    }
    let udevadm = which("udevadm")
        .ok_or_else(|| FbuildError::Other("udevadm not found on PATH".to_string()))?;

    // Private scratch dir: the helper and rules live there only for this call.
    let scratch = tempfile::Builder::new()
        .prefix("fbuild-udev-")
        .tempdir_in(fbuild_paths::temp_subdir("udev-install"))
        .map_err(|e| FbuildError::Other(format!("creating temp dir: {e}")))?;
    let helper = scratch.path().join(HELPER_NAME);
    let rules_tmp = scratch.path().join(UDEV_RULES_FILENAME);
    write_executable(&helper, HELPER_SCRIPT)?;
    std::fs::write(&rules_tmp, rules)
        .map_err(|e| FbuildError::Other(format!("writing {}: {e}", rules_tmp.display())))?;

    let argv = elevated_argv(
        &elevation,
        &helper,
        &dest,
        &rules_tmp,
        &dir.join(LEGACY_UDEV_RULES_FILENAME),
        &udevadm,
    );
    let argv_ref: Vec<&str> = argv.iter().map(String::as_str).collect();
    let askpass = match &elevation {
        Elevation::SudoAskpass { askpass, .. } => Some(askpass.display().to_string()),
        _ => None,
    };
    let overlay: Vec<(&str, &str)> = askpass
        .as_deref()
        .map(|a| vec![("SUDO_ASKPASS", a)])
        .unwrap_or_default();
    let out = fbuild_core::subprocess::run_command_blocking(
        &argv_ref,
        None,
        (!overlay.is_empty()).then_some(overlay.as_slice()),
        Some(std::time::Duration::from_secs(300)),
    )
    .map_err(|e| FbuildError::Other(format!("root prompt failed: {e}")))?;
    if !out.success() {
        return Err(FbuildError::Other(format!(
            "USB rules were not installed ({}). To do it yourself run:\n  {}",
            out.stderr.trim(),
            manual_command(&dest)
        )));
    }
    Ok(InstallOutcome::Installed)
}

/// Write `body` and mark it executable. It lives in the 0700 scratch dir, so
/// only this user (and root) can reach it whatever its own mode.
fn write_executable(path: &Path, body: &str) -> Result<()> {
    std::fs::write(path, body)
        .and_then(|()| fbuild_core::platform::fs::set_executable(path))
        .map_err(|e| FbuildError::Other(format!("writing {}: {e}", path.display())))
}

/// `fbuild port udev --install`.
pub fn run_install() -> Result<()> {
    super::port_scan::populate_online_overlay();
    let rules = render_current_rules()?;
    let env = HostEnv::probe();
    match install(&rules, &env)? {
        InstallOutcome::Installed => report_installed(&rules, &env),
        InstallOutcome::AlreadyCurrent => crate::output::result("udev rules already up to date"),
        InstallOutcome::Skipped => {}
    }
    Ok(())
}

/// NixOS: the `/run` install is lost at reboot; this is the permanent form.
pub fn nixos_snippet(rules: &str) -> String {
    let body: String = rules
        .lines()
        .filter(|l| l.starts_with("SUBSYSTEM=="))
        .map(|l| format!("    {l}\n"))
        .collect();
    format!("services.udev.extraRules = ''\n{body}'';\n")
}

/// `/dev/bus/usb/BBB/DDD` nodes of attached devices from registry vendors
/// that this user cannot open read-write. Opening a usbfs node does not
/// claim or reset the device (libusb does the same to enumerate), unlike
/// opening a tty, which can toggle DTR.
pub fn inaccessible_registry_devices(vids: &[u16]) -> Vec<NormalizedPath> {
    let Ok(entries) = std::fs::read_dir("/sys/bus/usb/devices") else {
        return Vec::new();
    };
    let read = |d: &Path, f: &str| std::fs::read_to_string(d.join(f)).ok();
    entries
        .flatten()
        .filter_map(|e| {
            let d = e.path();
            let vid = u16::from_str_radix(read(&d, "idVendor")?.trim(), 16).ok()?;
            if !vids.contains(&vid) {
                return None;
            }
            let bus: u32 = read(&d, "busnum")?.trim().parse().ok()?;
            let dev: u32 = read(&d, "devnum")?.trim().parse().ok()?;
            let node = NormalizedPath::from(format!("/dev/bus/usb/{bus:03}/{dev:03}"));
            let ok = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&node)
                .is_ok();
            (!ok).then_some(node)
        })
        .collect()
}

/// Preflight for deploy/monitor/debug: if an attached board cannot be opened
/// and the rules are missing or stale, install them (raising the root
/// prompt) when allowed, otherwise say how. Never fails the caller.
pub fn ensure_device_access() {
    if !fbuild_core::platform::host::is_linux() {
        return;
    }
    super::port_scan::populate_online_overlay();
    let vids = fbuild_core::usb::online_vendor_vids();
    let blocked = inaccessible_registry_devices(&vids);
    if blocked.is_empty() {
        return;
    }
    let env = HostEnv::probe();
    let Ok(rules) = render_current_rules() else {
        return;
    };
    if rules_current(Path::new(rules_dir(&env)), &rules) {
        return; // Rules are in place; the cause is elsewhere (e.g. SSH session).
    }
    let nodes: Vec<String> = blocked.iter().map(|p| p.display().to_string()).collect();
    if !may_prompt_automatically(&env) {
        crate::output::diagnostic(format!(
            "cannot open {} (no udev rules); run `fbuild port udev --install` once",
            nodes.join(", ")
        ));
        return;
    }
    crate::output::diagnostic(format!(
        "cannot open {} without root; offering to install USB rules \
         (set {NO_ELEVATE_ENV}=1 to never ask)",
        nodes.join(", ")
    ));
    match install(&rules, &env) {
        Ok(InstallOutcome::Installed) => report_installed(&rules, &env),
        Ok(_) => {}
        Err(e) => crate::output::diagnostic(format!("{e}")),
    }
}

/// Success message, plus the permanent NixOS form when we used `/run`.
pub fn report_installed(rules: &str, env: &HostEnv) {
    let dir = rules_dir(env);
    crate::output::result(format!(
        "udev rules installed at {dir}/{UDEV_RULES_FILENAME}"
    ));
    if env.nixos {
        crate::output::diagnostic(format!(
            "NixOS: {RUN_RULES_DIR} is cleared at reboot. To make this permanent add to \
             configuration.nix:\n{}",
            nixos_snippet(rules)
        ));
    }
}

fn which(name: &str) -> Option<NormalizedPath> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join(name))
            .find(|p| p.is_file())
            .map(NormalizedPath::from)
    })
}

fn is_root() -> bool {
    // /proc/self/status "Uid:\treal\teffective..."; no libc dependency needed.
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Uid:"))
                .and_then(|l| l.split_whitespace().nth(2).map(|e| e == "0"))
        })
        .unwrap_or(false)
}

#[cfg(test)]
#[path = "udev_install_tests.rs"]
mod tests;
