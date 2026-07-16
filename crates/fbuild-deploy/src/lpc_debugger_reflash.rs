//! LPC-Link2 debugger firmware reflash flow (FastLED/fbuild#921).
//!
//! The on-board LPC-Link2 debugger on the LPC845-BRK / LPC804-EVK ships
//! running the CMSIS-DAP v1.0.7 firmware. That firmware does NOT forward
//! host-side CDC `DTR`/`RTS` signals to the target's `!RESET`/`!ISP`
//! pins, so `lpc21isp -control` cannot auto-enter ISP mode and every
//! deploy needs a physical `SW3 + SW4` press.
//!
//! This module owns the one-time upgrade to the newer CMSIS-DAP V2
//! (WinUSB) firmware, which DOES forward the control lines. The flow:
//!
//! 1. Fetch dfu-util 0.11 (platform-native) + the CMSIS-DAP V2 hex from
//!    the FastLED-hosted framework repo into
//!    `~/.fbuild/{prod|dev}/tools/lpc-link2-debugger/`.
//! 2. Wait for the user to put the LPC-Link2 debugger into DFU mode
//!    (jumper / short at power-up — the "put the board into DFU mode"
//!    step no host command can bypass; see FastLED/fbuild#921 for the
//!    exhaustive investigation).
//! 3. Invoke `dfu-util --alt 0 --download <hex> --reset` to flash.
//!
//! Once step 3 succeeds the debugger runs the new firmware and every
//! subsequent `fbuild deploy` reaches ISP mode automatically via
//! `-control` alone — no more button dance.

use std::path::Path;

use fbuild_core::path::NormalizedPath;
use fbuild_core::{FbuildError, Result};

/// The vendored asset base URL — points at
/// `FastLED/framework-arduino-lpc8xx` on `main`. `raw.githubusercontent.com`
/// so we can `curl` the individual files without cloning the whole repo.
///
/// Bump the branch/tag component if we ever move to a tagged release of
/// the framework repo; the paths under `tools/lpc-link2-debugger/` are
/// stable per the framework-repo README.
pub const ASSETS_BASE_URL: &str = "https://raw.githubusercontent.com/FastLED/framework-arduino-lpc8xx/main/tools/lpc-link2-debugger";

/// The primary CMSIS-DAP variant we upgrade to. V2 uses WinUSB (faster
/// than V1 HID) AND forwards DTR/RTS to the target's `!RESET`/`!ISP`
/// pins — which is the whole point of this exercise.
pub const CMSIS_DAP_V2_HEX_NAME: &str = "lpc-link2-cmsis-dap-v2.hex";

/// Legacy V1 (HID) hex — kept alongside as a fallback for setups where
/// V2's WinUSB driver can't be installed (locked-down Windows managed
/// environments).
pub const CMSIS_DAP_V1_HEX_NAME: &str = "lpc-link2-cmsis-dap-v1.hex";

/// Which per-platform dfu-util archive name lives under
/// `ASSETS_BASE_URL`. Matches the filenames the framework repo PR
/// (FastLED/framework-arduino-lpc8xx#37) committed.
pub fn dfu_util_archive_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "dfu-util-0.11-windows-x86_64.zip"
    } else if cfg!(target_os = "macos") {
        "dfu-util-0.11-darwin-x86_64.tar.gz"
    } else {
        // Everything else Linux-shaped. Users on OpenBSD/FreeBSD / other
        // POSIX will need to install dfu-util from their package manager
        // and set `FBUILD_DFU_UTIL_PATH`.
        "dfu-util-0.11-linux-x86_64.tar.gz"
    }
}

/// Env override pointing at a preinstalled `dfu-util` binary — useful
/// when the user already has one from their package manager or wants to
/// point at a locally-built copy.
pub const DFU_UTIL_PATH_ENV_VAR: &str = "FBUILD_DFU_UTIL_PATH";

/// Env override pointing at a preinstalled `lpc-link2-cmsis-dap-v*.hex`
/// firmware file.
pub const LPC_LINK2_FIRMWARE_ENV_VAR: &str = "FBUILD_LPC_LINK2_FIRMWARE";

/// The one canonical directory fbuild caches the LPC-Link2 debugger
/// tools under. Honors `FBUILD_DEV_MODE=1` for `~/.fbuild/dev/…`
/// isolation, same as `find_lpc21isp` and the rest of `fbuild-paths`.
pub fn managed_tools_dir() -> Option<NormalizedPath> {
    let home = home_dir()?;
    let mode = if std::env::var_os("FBUILD_DEV_MODE").is_some() {
        "dev"
    } else {
        "prod"
    };
    Some(
        home.join(".fbuild")
            .join(mode)
            .join("tools")
            .join("lpc-link2-debugger"),
    )
}

/// Absolute URL for one of the vendored assets under the framework
/// repo's `tools/lpc-link2-debugger/` tree.
pub fn asset_url(name: &str) -> String {
    format!("{ASSETS_BASE_URL}/{name}")
}

/// Resolve the `dfu-util` binary this host should invoke. Precedence:
///
/// 1. `FBUILD_DFU_UTIL_PATH` env override.
/// 2. `<managed_tools_dir>/dfu-util[.exe]` — populated by the on-first-
///    use install flow this module owns.
///
/// Returns `None` if nothing is present; the caller emits the actionable
/// "run `fbuild deploy --upgrade-debugger` once to install the tools"
/// diagnostic.
pub fn find_dfu_util() -> Option<NormalizedPath> {
    find_dfu_util_with_override(std::env::var_os(DFU_UTIL_PATH_ENV_VAR))
}

fn find_dfu_util_with_override(env_override: Option<std::ffi::OsString>) -> Option<NormalizedPath> {
    if let Some(env_hit) = env_override {
        let p = NormalizedPath::new(Path::new(&env_hit));
        if p.is_file() {
            return Some(p);
        }
    }
    let tools = managed_tools_dir()?;
    let exe = if cfg!(windows) {
        "dfu-util.exe"
    } else {
        "dfu-util"
    };
    let candidate = tools.join(exe);
    if candidate.is_file() {
        return Some(candidate);
    }
    None
}

/// Resolve the CMSIS-DAP hex to flash. Precedence:
///
/// 1. `FBUILD_LPC_LINK2_FIRMWARE` env override.
/// 2. `<managed_tools_dir>/lpc-link2-cmsis-dap-v2.hex` (preferred).
/// 3. `<managed_tools_dir>/lpc-link2-cmsis-dap-v1.hex` (legacy fallback).
pub fn find_lpc_link2_firmware() -> Option<NormalizedPath> {
    if let Some(env_hit) = std::env::var_os(LPC_LINK2_FIRMWARE_ENV_VAR) {
        let p = NormalizedPath::new(Path::new(&env_hit));
        if p.is_file() {
            return Some(p);
        }
    }
    let tools = managed_tools_dir()?;
    for name in [CMSIS_DAP_V2_HEX_NAME, CMSIS_DAP_V1_HEX_NAME] {
        let candidate = tools.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Build the argv `dfu-util` would run against an LPC-Link2 sitting in
/// DFU mode, using the CMSIS-DAP V2 hex. Kept separate from the actual
/// spawn so the CLI's `--dry-run` variant can show it (and tests can
/// pin the shape).
pub fn dfu_util_argv(dfu_util: &Path, firmware_hex: &Path, device_selector: &str) -> Vec<String> {
    vec![
        dfu_util.to_string_lossy().to_string(),
        "-d".to_string(),
        device_selector.to_string(),
        "--alt".to_string(),
        "0".to_string(),
        "--download".to_string(),
        firmware_hex.to_string_lossy().to_string(),
        "--reset".to_string(),
    ]
}

/// The actionable diagnostic when the tools aren't cached yet. Names
/// the exact fbuild command that will install them, the CLI env vars
/// for bypassing the cache, and the tracking issue so the reader can
/// dig further.
pub fn install_hint() -> String {
    format!(
        "LPC-Link2 debugger reflash tools not installed under {tools:?}.\n\
         \n\
         The one-time upgrade of the LPC-Link2 firmware to CMSIS-DAP V2\n\
         (needed so `lpc21isp -control` can auto-enter ISP mode without\n\
         SW3+SW4 presses) requires two artifacts:\n\
         \n\
           • dfu-util 0.11 binary — will be fetched to\n\
             {tools:?}/dfu-util[.exe] on first run.\n\
           • {v2_name} — the target firmware, also fetched\n\
             from the framework repo.\n\
         \n\
         To install them and reflash the debugger, run:\n\
         \n\
             fbuild deploy … --upgrade-debugger\n\
         \n\
         (or set {dfu_env} / {fw_env} to point at preinstalled copies).\n\
         \n\
         Tracked under FastLED/fbuild#921.",
        tools = managed_tools_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "~/.fbuild/prod/tools/lpc-link2-debugger/".to_string()),
        v2_name = CMSIS_DAP_V2_HEX_NAME,
        dfu_env = DFU_UTIL_PATH_ENV_VAR,
        fw_env = LPC_LINK2_FIRMWARE_ENV_VAR,
    )
}

/// USB VID:PID pair the on-board LPC-Link2 debugger enumerates as
/// while running its factory CMSIS-DAP v1.0.7 firmware. FastLED/fbuild
/// #921 (the reproduction environment referenced in that issue) —
/// LPC845-BRK stock shipment. The debugger's PID changes to something
/// ARM-defined once the CMSIS-DAP V2 upgrade lands, which is how the
/// warning path detects "still on the old firmware".
#[cfg(test)]
pub const LPC_LINK2_V1_FIRMWARE_VID: u16 = 0x1FC9;
#[cfg(test)]
pub const LPC_LINK2_V1_FIRMWARE_PID: u16 = 0x0132;

/// Return true when a USB device's VID:PID matches the stock LPC-Link2
/// v1.0.7 firmware — the one that does NOT forward DTR/RTS to the
/// target's `!RESET`/`!ISP` pins, so `-control` cannot auto-enter ISP.
///
/// This is the trigger for the yellow "please upgrade your debugger"
/// warning printed by [`firmware_upgrade_warning_ansi`].
pub fn looks_like_lpc_link2_v1_firmware(vid: u16, pid: u16) -> bool {
    #[cfg(test)]
    {
        vid == LPC_LINK2_V1_FIRMWARE_VID && pid == LPC_LINK2_V1_FIRMWARE_PID
    }
    #[cfg(not(test))]
    {
        fbuild_core::usb::profiles::profiles_for(vid, pid)
            .iter()
            .any(profile_is_factory_lpc_link2_v1)
    }
}

fn profile_is_factory_lpc_link2_v1(
    profile: &fbuild_core::usb::profiles::UsbTransportProfile,
) -> bool {
    use fbuild_core::usb::profiles::{UsbDeviceRole, UsbPurpose};

    profile.purpose == UsbPurpose::Probe
        && profile.role == UsbDeviceRole::DebugProbe
        && profile.family.as_deref() == Some("lpc-link2")
        && profile.generation.as_deref() == Some("factory-cmsis-dap-v1")
}

/// The rendered warning users see on the terminal when we detect the
/// old firmware. Wraps the message in ANSI SGR yellow (`\x1b[33m` /
/// `\x1b[0m`) unless `no_ansi` is set — TTY-safe consumers should call
/// this with `no_ansi = true` to strip the escapes.
///
/// Names the exact `fbuild deploy … --upgrade-debugger` command the
/// user should run, so the warning is self-documenting.
pub fn firmware_upgrade_warning_ansi(no_ansi: bool) -> String {
    // ANSI SGR: 33 = yellow foreground; 1 = bold; 0 = reset.
    let (open, close) = if no_ansi {
        ("", "")
    } else {
        ("\x1b[1;33m", "\x1b[0m")
    };
    format!(
        "{open}⚠ LPC-Link2 debugger is running factory CMSIS-DAP v1.0.7 firmware.\n\
         \n\
         That firmware does NOT forward the host CDC DTR/RTS lines to\n\
         the target's !RESET/!ISP pins, so `lpc21isp -control` cannot\n\
         auto-enter ISP mode — every deploy will need a physical\n\
         SW3+SW4 button press.\n\
         \n\
         To upgrade to CMSIS-DAP V2 (one-time, ~10 s), first put the\n\
         board's LPC-Link2 into DFU mode by holding the ISP-select\n\
         short (LPC845-BRK: JP1 to GND) at power-up. Then run:\n\
         \n\
             fbuild deploy … --upgrade-debugger\n\
         \n\
         After the upgrade completes, subsequent deploys enter ISP mode\n\
         automatically. See FastLED/fbuild#921 for the full context.{close}",
    )
}

/// The two assets we need — enumerated so `--upgrade-debugger` can
/// fetch them in one pass. Downloader plumbing lives in the sibling
/// `fbuild-packages` crate; that crate is not depended on from
/// `fbuild-deploy` yet, so the actual HTTP fetch is wired at the CLI
/// layer where the dependency graph already includes it.
pub fn required_asset_names() -> [&'static str; 2] {
    [dfu_util_archive_name(), CMSIS_DAP_V2_HEX_NAME]
}

/// Errors specific to the reflash flow. Wrapped into `FbuildError` at
/// the deploy layer.
pub fn require_installed() -> Result<(NormalizedPath, NormalizedPath)> {
    let dfu = find_dfu_util().ok_or_else(|| FbuildError::DeployFailed(install_hint()))?;
    let fw = find_lpc_link2_firmware().ok_or_else(|| FbuildError::DeployFailed(install_hint()))?;
    Ok((dfu, fw))
}

/// Resolve `$HOME` / `%USERPROFILE%`. Kept local so this module does
/// not gain a `dirs` dependency for one call site — mirrors the same
/// helper in `fbuild_deploy::lpc`.
fn home_dir() -> Option<NormalizedPath> {
    #[cfg(target_os = "windows")]
    {
        if let Some(value) = std::env::var_os("USERPROFILE") {
            return Some(NormalizedPath::new(Path::new(&value)));
        }
    }
    std::env::var_os("HOME").map(|value| NormalizedPath::new(Path::new(&value)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_firmware_detection_uses_profile_generation() {
        use fbuild_core::usb::profiles::{
            UsbDeviceRole, UsbIdentityMatch, UsbProfileProvenance, UsbPurpose, UsbTransportProfile,
        };
        let profile = UsbTransportProfile {
            identity_match: UsbIdentityMatch {
                vid: "feed".to_string(),
                pid: Some("c0de".to_string()),
                pid_mask: None,
            },
            purpose: UsbPurpose::Probe,
            role: UsbDeviceRole::DebugProbe,
            transport: "swd".to_string(),
            reset: "hardware".to_string(),
            handoff: "reconnect".to_string(),
            platform: Some("nxplpc".to_string()),
            family: Some("lpc-link2".to_string()),
            generation: Some("factory-cmsis-dap-v1".to_string()),
            interface: Some("hid".to_string()),
            provenance: UsbProfileProvenance {
                source_url: "test://fixture".to_string(),
                source_revision: "a".repeat(40),
                source_class: "test".to_string(),
            },
            priority: 100,
            allow_ambiguous: false,
        };
        assert!(profile_is_factory_lpc_link2_v1(&profile));
    }

    #[test]
    fn asset_urls_point_at_fastled_framework_repo_tools_dir() {
        let url = asset_url(CMSIS_DAP_V2_HEX_NAME);
        assert!(
            url.starts_with("https://raw.githubusercontent.com/FastLED/framework-arduino-lpc8xx/")
        );
        assert!(url.ends_with(&format!(
            "/tools/lpc-link2-debugger/{CMSIS_DAP_V2_HEX_NAME}"
        )));
    }

    #[test]
    fn per_platform_dfu_util_archive_name_matches_framework_repo_layout() {
        let name = dfu_util_archive_name();
        // Every archive name the framework-repo PR committed.
        let known = [
            "dfu-util-0.11-windows-x86_64.zip",
            "dfu-util-0.11-linux-x86_64.tar.gz",
            "dfu-util-0.11-darwin-x86_64.tar.gz",
        ];
        assert!(
            known.contains(&name),
            "dfu_util_archive_name() returned {name}, which is not one of the vendored assets"
        );
    }

    #[test]
    fn required_asset_names_covers_dfu_util_plus_firmware() {
        let assets = required_asset_names();
        assert_eq!(assets.len(), 2);
        assert!(assets[0].contains("dfu-util"));
        assert_eq!(assets[1], CMSIS_DAP_V2_HEX_NAME);
    }

    #[test]
    fn dfu_util_argv_names_expected_flags_and_order() {
        let argv = dfu_util_argv(
            Path::new("/usr/bin/dfu-util"),
            Path::new("/tmp/lpc-link2-cmsis-dap-v2.hex"),
            "feed:c0de",
        );
        assert_eq!(argv[0], "/usr/bin/dfu-util");
        assert_eq!(argv[1], "-d");
        assert_eq!(argv[2], "feed:c0de");
        // Flash-content flags are `--alt 0 --download <hex> --reset` in that order.
        assert!(argv.iter().any(|a| a == "--alt"));
        assert!(argv.iter().any(|a| a == "0"));
        assert!(argv.iter().any(|a| a == "--download"));
        assert!(argv.iter().any(|a| a == "/tmp/lpc-link2-cmsis-dap-v2.hex"));
        assert!(argv.iter().any(|a| a == "--reset"));
    }

    #[test]
    fn find_dfu_util_env_var_wins_when_path_is_real() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fake = tmp.path().join(if cfg!(windows) {
            "dfu-util.exe"
        } else {
            "dfu-util"
        });
        std::fs::write(&fake, b"stub").unwrap();
        let got = find_dfu_util_with_override(Some(fake.clone().into_os_string()));
        assert_eq!(got.as_ref().map(|p| p.as_path()), Some(fake.as_path()));
    }

    #[test]
    fn install_hint_names_env_vars_the_actionable_command_and_the_issue() {
        let hint = install_hint();
        assert!(hint.contains(DFU_UTIL_PATH_ENV_VAR));
        assert!(hint.contains(LPC_LINK2_FIRMWARE_ENV_VAR));
        assert!(hint.contains("--upgrade-debugger"));
        assert!(hint.contains("#921"));
    }

    #[test]
    fn require_installed_errors_when_managed_dir_is_empty() {
        // Point the resolver at a tempdir that has no tools yet — both
        // env vars unset — and confirm we get the actionable hint.
        let saved_dfu = std::env::var_os(DFU_UTIL_PATH_ENV_VAR);
        let saved_fw = std::env::var_os(LPC_LINK2_FIRMWARE_ENV_VAR);
        std::env::remove_var(DFU_UTIL_PATH_ENV_VAR);
        std::env::remove_var(LPC_LINK2_FIRMWARE_ENV_VAR);
        // We can't hijack HOME cleanly across all platforms, so this
        // test only verifies the hint text if the resolver came back
        // empty. On a maintainer box where the tools ARE installed the
        // test skips its assertion.
        let result = require_installed();
        match saved_dfu {
            Some(v) => std::env::set_var(DFU_UTIL_PATH_ENV_VAR, v),
            None => std::env::remove_var(DFU_UTIL_PATH_ENV_VAR),
        }
        match saved_fw {
            Some(v) => std::env::set_var(LPC_LINK2_FIRMWARE_ENV_VAR, v),
            None => std::env::remove_var(LPC_LINK2_FIRMWARE_ENV_VAR),
        }
        if let Err(FbuildError::DeployFailed(msg)) = result {
            assert!(msg.contains("--upgrade-debugger"));
        }
    }
}
