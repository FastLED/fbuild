//! probe-rs SWD flash path (FastLED/fbuild#935, #936).
//!
//! The FastLED fork of `probe-rs` — sources maintained on the `tools`
//! branch of [`FastLED/framework-arduino-lpc8xx`], cross-compiled by
//! `.github/workflows/fastled-release-cross.yml` — carries three
//! patches on top of upstream that together allow it to talk to the
//! LPC-Link2 v1.0.7 CMSIS-DAP firmware that ships on the LPC845-BRK:
//!
//! 1. FastLED/probe-rs#1 — `DAP_Info` sub-command fallbacks (packet
//!    size / count / capabilities) → spec-default 64-byte HID reports
//!    when the firmware doesn't answer the query.
//! 2. FastLED/probe-rs#2 — Explicit `DAP_Connect(Swd)` + 4× retry
//!    instead of the optional `DefaultPort` (which the v1.0.7 firmware
//!    doesn't implement).
//! 3. FastLED/fbuild#936 — nusb-based CMSIS-DAP v1 HID transport as
//!    a `CmsisDapDevice::V1Nusb` variant, dispatched between the v2
//!    (bulk) attempt and the hidapi fallback. On Windows the hidapi
//!    fallback is the winning path because HidUsb owns interface 0;
//!    on Linux/macOS the nusb path wins uncontested.
//!
//! End-to-end validation: the LPC845-BRK on the maintainer's Windows
//! machine flashes touchless in ~2 s via
//!
//! ```text
//! probe-rs.exe download --chip LPC845M301JBD48 \
//!                       --probe 1fc9:0132 \
//!                       firmware.elf
//! ```
//!
//! This module wraps that invocation so [`crate::lpc::LpcDeployer`]
//! can dispatch to it in preference to the UART-ISP path (lpc21isp),
//! which requires a `SW3 + SW4` button press to enter ISP mode.

use std::path::{Path, PathBuf};
use std::time::Duration;

use fbuild_core::subprocess::run_command_blocking;
use fbuild_core::{FbuildError, Result};

/// Environment override that points at a specific `probe-rs` binary.
/// Primarily useful during development against a locally-built
/// probe-rs before the auto-download landing.
pub const PROBE_RS_PATH_ENV_VAR: &str = "FBUILD_PROBE_RS_PATH";

/// Hard ceiling on a single probe-rs invocation. A healthy LPC845-BRK
/// flash completes in ~2 s; 120 s covers a slow cold HID enumerate plus
/// full-chip program with margin. Past that, the probe is wedged and
/// the deploy should fail with the captured stderr, not hang the
/// daemon's spawn_blocking slot indefinitely.
const PROBE_RS_TIMEOUT: Duration = Duration::from_secs(120);

/// Locally-built probe-rs binary — the maintainer's dev tree. Kept as
/// a documented convention so ad-hoc smoke tests can just drop the
/// binary in a well-known place instead of remembering the env var.
///
/// Honors `FBUILD_DEV_MODE=1` → `~/.fbuild/dev/tools/probe-rs/` to
/// match the isolation the rest of `fbuild-paths` applies.
pub fn managed_probe_rs_path() -> Option<PathBuf> {
    let exe = if cfg!(windows) {
        "probe-rs.exe"
    } else {
        "probe-rs"
    };
    let home = home_dir_local()?;
    let mode = if std::env::var_os("FBUILD_DEV_MODE").is_some() {
        "dev"
    } else {
        "prod"
    };
    Some(
        home.join(".fbuild")
            .join(mode)
            .join("tools")
            .join("probe-rs")
            .join(exe),
    )
}

/// Resolve which `probe-rs` binary to invoke, or `None` when neither
/// the env override nor the managed path point at an existing file.
/// The caller decides whether that's a hard-fail or a fall-through to
/// [`crate::lpc::LpcDeployer`]'s lpc21isp path.
///
/// Search order (first hit wins):
///
/// 1. `FBUILD_PROBE_RS_PATH` — explicit override.
/// 2. `~/.fbuild/{prod|dev}/tools/probe-rs/probe-rs[.exe]` — the
///    canonical fbuild-managed location the auto-download will
///    populate (follow-up).
///
/// Deliberately does NOT walk `PATH` — a system `probe-rs` from a
/// distro package will lack the FastLED patches and will hang on the
/// LPC-Link2 v1.0.7 firmware, which is the exact failure mode this
/// module exists to avoid.
pub fn find_probe_rs() -> Option<PathBuf> {
    if let Some(env_hit) = std::env::var_os(PROBE_RS_PATH_ENV_VAR) {
        let p = PathBuf::from(env_hit);
        if p.is_file() {
            return Some(p);
        }
    }

    let managed = managed_probe_rs_path()?;
    if managed.is_file() {
        return Some(managed);
    }
    None
}

/// Map an fbuild `BoardConfig` to the `--chip` name probe-rs expects.
///
/// probe-rs uses the exact silicon SKU (e.g. `LPC845M301JBD48`) rather
/// than the board-nickname convention that fbuild inherits from
/// PlatformIO (`lpc845brk` → the whole eval board). The mapping is
/// hardcoded here rather than added to every board JSON so we can
/// keep the JSON schema stable; when probe-rs gains support for more
/// NXP-family chips, extend this table in one place.
///
/// Returns `None` for boards whose chip probe-rs doesn't know about —
/// the caller falls back to the lpc21isp UART ISP path.
pub fn map_board_to_probe_rs_chip(board: &fbuild_config::BoardConfig) -> Option<&'static str> {
    // fbuild's BoardConfig `mcu` field is the PlatformIO-style short
    // name (e.g. `lpc845`, `lpc812`). Match on that plus `board.board`
    // for boards where the short-name maps to multiple SKUs.
    let mcu = board.mcu.to_ascii_lowercase();
    match mcu.as_str() {
        // LPC845-BRK ships the LPC845M301JBD48 variant.
        "lpc845" => Some("LPC845M301JBD48"),
        // Extend here as new probe-rs-supported chips get board JSONs.
        _ => None,
    }
}

/// Argv builder for `probe-rs download`. Kept as a pure function so
/// tests can assert the exact argv without hitting real hardware.
pub fn probe_rs_download_argv(
    probe_rs_path: &Path,
    chip: &str,
    probe_selector: Option<&str>,
    firmware_path: &Path,
) -> Vec<String> {
    let mut args = vec![
        probe_rs_path.to_string_lossy().to_string(),
        "download".to_string(),
        "--chip".to_string(),
        chip.to_string(),
    ];
    if let Some(sel) = probe_selector {
        args.push("--probe".to_string());
        args.push(sel.to_string());
    }
    args.push(firmware_path.to_string_lossy().to_string());
    args
}

/// Argv builder for `probe-rs reset` — separated so callers can chain
/// a reset after `download` when the target needs to start executing
/// from a clean state.
pub fn probe_rs_reset_argv(
    probe_rs_path: &Path,
    chip: &str,
    probe_selector: Option<&str>,
) -> Vec<String> {
    let mut args = vec![
        probe_rs_path.to_string_lossy().to_string(),
        "reset".to_string(),
        "--chip".to_string(),
        chip.to_string(),
    ];
    if let Some(sel) = probe_selector {
        args.push("--probe".to_string());
        args.push(sel.to_string());
    }
    args
}

/// Detect whether an LPC-Link2 CMSIS-DAP probe is currently attached
/// by scanning available serial ports for the LPC-Link2's VID:PID.
/// The debugger enumerates as a USB composite whose CDC side (COM
/// port) carries the same VID:PID as the CMSIS-DAP HID interface, so
/// a match on the serial-port list is a reliable proxy for probe
/// presence — and lets us stay on the crate's existing `serialport`
/// dep without pulling in a whole USB library just for enumeration.
///
/// Matches on the two LPC-Link2 VID:PID pairs that carry standard
/// CMSIS-DAP HID firmware:
///
/// - `1FC9:0090` — standalone LPC-Link2 dongle.
/// - `1FC9:0132` — on-board LPC-Link2 (LPC845-BRK v1.0.7 firmware).
///
/// Both are the exact set the probe-rs interface-picker patch
/// (FastLED/fbuild#935) explicitly whitelists.
pub fn lpc_link2_probe_attached() -> bool {
    lpc_link2_probe_selector().is_some()
}

/// Compute the `--probe` selector string for the first attached
/// LPC-Link2, in the `VID:PID` shorthand probe-rs accepts (e.g.
/// `1fc9:0132`). Returns `None` when no probe is present.
///
/// Serial-number disambiguation is intentionally omitted — the
/// LPC845-BRK is the only debugger this dispatch path ever runs
/// against in fbuild, so a plain VID:PID match is unambiguous.
pub fn lpc_link2_probe_selector() -> Option<String> {
    let Ok(ports) = serialport::available_ports() else {
        return None;
    };
    for p in ports {
        let serialport::SerialPortType::UsbPort(usb) = &p.port_type else {
            continue;
        };
        if usb.vid == 0x1fc9 && matches!(usb.pid, 0x0090 | 0x0132) {
            return Some(format!("{:04x}:{:04x}", usb.vid, usb.pid));
        }
    }
    None
}

/// Blocking wrapper around `probe-rs download` returning a structured
/// stdout/stderr + exit-code triple. Used by the LpcDeployer async
/// dispatch, which spawns this on `spawn_blocking` so the tokio
/// runtime isn't held while probe-rs waits on USB. FastLED/fbuild#935.
pub fn run_probe_rs_download(
    probe_rs_path: &Path,
    chip: &str,
    probe_selector: Option<&str>,
    firmware_path: &Path,
) -> Result<ProbeRsRun> {
    let argv = probe_rs_download_argv(probe_rs_path, chip, probe_selector, firmware_path);
    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    // run_command_blocking (not raw std::process::Command) so the spawn
    // inherits CREATE_NO_WINDOW on Windows — the daemon is windowless,
    // and a raw console-subsystem child pops a visible console for the
    // duration of the flash. Also buys the standard timeout guard: a
    // wedged probe fails the deploy in 120 s instead of hanging it.
    let output = run_command_blocking(&argv_refs, None, None, Some(PROBE_RS_TIMEOUT))
        .map_err(|e| FbuildError::DeployFailed(format!("failed to spawn probe-rs: {e}")))?;
    Ok(ProbeRsRun {
        exit_code: output.exit_code,
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

/// Same for `probe-rs reset` — sequenced after a successful download
/// so the newly-flashed firmware starts running immediately.
pub fn run_probe_rs_reset(
    probe_rs_path: &Path,
    chip: &str,
    probe_selector: Option<&str>,
) -> Result<ProbeRsRun> {
    let argv = probe_rs_reset_argv(probe_rs_path, chip, probe_selector);
    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    // Same CREATE_NO_WINDOW + timeout rationale as run_probe_rs_download.
    let output = run_command_blocking(&argv_refs, None, None, Some(PROBE_RS_TIMEOUT))
        .map_err(|e| FbuildError::DeployFailed(format!("failed to spawn probe-rs reset: {e}")))?;
    Ok(ProbeRsRun {
        exit_code: output.exit_code,
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

/// Return of [`run_probe_rs_download`] / [`run_probe_rs_reset`].
#[derive(Debug, Clone)]
pub struct ProbeRsRun {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl ProbeRsRun {
    pub fn success(&self) -> bool {
        self.exit_code == 0
    }
}

fn home_dir_local() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        if let Some(v) = std::env::var_os("USERPROFILE") {
            return Some(PathBuf::from(v));
        }
    }
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_argv_shape() {
        let argv = probe_rs_download_argv(
            Path::new("/tmp/probe-rs"),
            "LPC845M301JBD48",
            Some("1fc9:0132"),
            Path::new("/tmp/firmware.elf"),
        );
        assert_eq!(argv[0], "/tmp/probe-rs");
        assert_eq!(argv[1], "download");
        assert_eq!(argv[2], "--chip");
        assert_eq!(argv[3], "LPC845M301JBD48");
        assert_eq!(argv[4], "--probe");
        assert_eq!(argv[5], "1fc9:0132");
        assert_eq!(argv[6], "/tmp/firmware.elf");
    }

    #[test]
    fn download_argv_without_probe_selector() {
        let argv = probe_rs_download_argv(
            Path::new("probe-rs"),
            "LPC845M301JBD48",
            None,
            Path::new("firmware.elf"),
        );
        assert_eq!(argv.len(), 5);
        assert_eq!(argv[4], "firmware.elf");
    }

    #[test]
    fn reset_argv_shape() {
        let argv = probe_rs_reset_argv(
            Path::new("/x/probe-rs"),
            "LPC845M301JBD48",
            Some("1fc9:0132"),
        );
        assert_eq!(
            argv,
            vec![
                "/x/probe-rs".to_string(),
                "reset".to_string(),
                "--chip".to_string(),
                "LPC845M301JBD48".to_string(),
                "--probe".to_string(),
                "1fc9:0132".to_string(),
            ]
        );
    }

    #[test]
    fn lpc845_maps_to_probe_rs_chip() {
        let board = fbuild_config::BoardConfig {
            name: "lpc845brk".to_string(),
            mcu: "lpc845".to_string(),
            ..Default::default()
        };
        assert_eq!(map_board_to_probe_rs_chip(&board), Some("LPC845M301JBD48"));
    }

    #[test]
    fn unknown_board_returns_none() {
        let board = fbuild_config::BoardConfig {
            name: "nothing".to_string(),
            mcu: "unknown_mcu".to_string(),
            ..Default::default()
        };
        assert_eq!(map_board_to_probe_rs_chip(&board), None);
    }
}
