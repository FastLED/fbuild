//! probe-rs SWD flash path (FastLED/fbuild#935, #936).
//!
//! The FastLED fork of `probe-rs` — sources maintained on the `tools`
//! branch of `FastLED/framework-arduino-lpc8xx`, cross-compiled by
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

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fbuild_core::path::NormalizedPath;
use fbuild_core::subprocess::run_command_blocking;
use fbuild_core::{FbuildError, Result};

/// Environment override that points at a specific `probe-rs` binary.
/// Primarily useful during development against a locally-built
/// probe-rs before the auto-download landing.
pub const PROBE_RS_PATH_ENV_VAR: &str = "FBUILD_PROBE_RS_PATH";

/// Pinned FastLED/probe-rs release consumed by fbuild. The patched
/// fork and cross-compile workflow live in FastLED/framework-arduino-lpc8xx;
/// fbuild only downloads and verifies the host artifact.
pub const PROBE_RS_RELEASE_TAG: &str = "fastled-v0.31.2-nusb-v1-transport";

const PROBE_RS_RELEASE_DOWNLOAD_BASE: &str =
    "https://github.com/FastLED/framework-arduino-lpc8xx/releases/download";

/// Hard ceiling on a single probe-rs invocation. A healthy LPC845-BRK
/// flash completes in ~2 s; 120 s covers a slow cold HID enumerate plus
/// full-chip program with margin. Past that, the probe is wedged and
/// the deploy should fail with the captured stderr, not hang the
/// daemon's spawn_blocking slot indefinitely.
const PROBE_RS_TIMEOUT: Duration = Duration::from_secs(120);

/// Release asset metadata for the current host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeRsReleaseAsset {
    pub name: &'static str,
    pub sha256: &'static str,
}

impl ProbeRsReleaseAsset {
    pub fn url(&self) -> String {
        format!(
            "{}/{}/{}",
            PROBE_RS_RELEASE_DOWNLOAD_BASE, PROBE_RS_RELEASE_TAG, self.name
        )
    }
}

/// Return the pinned FastLED/probe-rs release asset for this host.
pub fn probe_rs_release_asset_for_host() -> Result<ProbeRsReleaseAsset> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Ok(ProbeRsReleaseAsset {
            name: "probe-rs-fastled-fastled-v0.31.2-nusb-v1-transport-x86_64-pc-windows-msvc.zip",
            sha256: "257e294988498218cf350a852bf60e57313b19f492f8019b92905df59095c7a1",
        }),
        ("windows", "aarch64") => Ok(ProbeRsReleaseAsset {
            name: "probe-rs-fastled-fastled-v0.31.2-nusb-v1-transport-aarch64-pc-windows-msvc.zip",
            sha256: "f3bd8117b6a1de3e73791aa0ec980ed07a18482fffbc43b70af57750450cd854",
        }),
        ("linux", "x86_64") => Ok(ProbeRsReleaseAsset {
            name: "probe-rs-fastled-fastled-v0.31.2-nusb-v1-transport-x86_64-unknown-linux-gnu.tar.zst",
            sha256: "acd37d2a012fd7e12d83e951ea2489dcbc76011b6b9af0a8214ef399a8ec401b",
        }),
        ("linux", "aarch64") => Ok(ProbeRsReleaseAsset {
            name: "probe-rs-fastled-fastled-v0.31.2-nusb-v1-transport-aarch64-unknown-linux-gnu.tar.zst",
            sha256: "ea921ea77709640b4947c68646154a0e7e3edcdb6cd9f95270f28599929d0ac6",
        }),
        ("macos", "x86_64") => Ok(ProbeRsReleaseAsset {
            name: "probe-rs-fastled-fastled-v0.31.2-nusb-v1-transport-x86_64-apple-darwin.tar.zst",
            sha256: "d106888b816854c29b77af4e67982d6da1c9e2ef7c8203fa478adfb4065699df",
        }),
        ("macos", "aarch64") => Ok(ProbeRsReleaseAsset {
            name: "probe-rs-fastled-fastled-v0.31.2-nusb-v1-transport-aarch64-apple-darwin.tar.zst",
            sha256: "36e4dd61804a438eea37dbc68d62bc175c679af38c45f6008cd248169f20d2b5",
        }),
        (os, arch) => Err(FbuildError::PackageError(format!(
            "no FastLED probe-rs artifact is published for {os}/{arch}"
        ))),
    }
}

/// fbuild-managed probe-rs install directory.
///
/// Honors `FBUILD_DEV_MODE=1` → `~/.fbuild/dev/tools/probe-rs/` to
/// match the isolation the rest of `fbuild-paths` applies.
pub fn managed_probe_rs_dir() -> Option<NormalizedPath> {
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
            .join("probe-rs"),
    )
}

pub fn managed_probe_rs_path() -> Option<NormalizedPath> {
    let exe = if cfg!(windows) {
        "probe-rs.exe"
    } else {
        "probe-rs"
    };
    Some(managed_probe_rs_dir()?.join(exe))
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
///    canonical fbuild-managed location populated from the pinned
///    FastLED/probe-rs release.
///
/// Deliberately does NOT walk `PATH` — a system `probe-rs` from a
/// distro package will lack the FastLED patches and will hang on the
/// LPC-Link2 v1.0.7 firmware, which is the exact failure mode this
/// module exists to avoid.
pub fn find_probe_rs() -> Option<NormalizedPath> {
    if let Some(env_hit) = std::env::var_os(PROBE_RS_PATH_ENV_VAR) {
        let p = NormalizedPath::new(Path::new(&env_hit));
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

/// Resolve and, if needed, install the pinned FastLED/probe-rs binary.
pub async fn ensure_probe_rs_installed() -> Result<NormalizedPath> {
    if let Some(existing) = find_probe_rs() {
        return Ok(existing);
    }

    install_managed_probe_rs().await
}

/// Download, verify, extract, and install the pinned FastLED/probe-rs
/// release artifact into the fbuild-managed tools directory.
pub async fn install_managed_probe_rs() -> Result<NormalizedPath> {
    let asset = probe_rs_release_asset_for_host()?;
    let dest = managed_probe_rs_path().ok_or_else(|| {
        FbuildError::PackageError("could not determine managed probe-rs path".to_string())
    })?;

    let staging_dir = probe_rs_staging_dir();
    tokio::fs::create_dir_all(&staging_dir).await?;

    let url = asset.url();
    tracing::info!("installing FastLED probe-rs from {}", url);
    let archive = fbuild_packages::downloader::download_file(&url, &staging_dir).await?;

    fbuild_packages::downloader::verify_checksum_async(&archive, asset.sha256).await?;

    let dest_path = dest.clone().into_path_buf();
    let installed = tokio::task::spawn_blocking(move || {
        extract_and_install_probe_rs(&archive, &staging_dir, &dest_path)
    })
    .await
    .map_err(|e| FbuildError::PackageError(format!("probe-rs install task failed: {e}")))??;

    Ok(NormalizedPath::new(installed))
}

fn probe_rs_staging_dir() -> NormalizedPath {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    NormalizedPath::from(fbuild_paths::temp_subdir("probe-rs-install")).join(format!(
        "{}-{}-{}",
        PROBE_RS_RELEASE_TAG,
        std::process::id(),
        millis
    ))
}

fn extract_and_install_probe_rs(
    archive: &Path,
    staging_dir: &Path,
    dest_path: &Path,
) -> Result<NormalizedPath> {
    let extract_dir = staging_dir.join("extract");
    std::fs::create_dir_all(&extract_dir)?;
    fbuild_packages::extractor::extract(archive, &extract_dir)?;

    let found = find_extracted_probe_rs_binary(&extract_dir)?;
    let dest_dir = dest_path.parent().ok_or_else(|| {
        FbuildError::PackageError(format!(
            "managed probe-rs path has no parent: {}",
            dest_path.display()
        ))
    })?;
    std::fs::create_dir_all(dest_dir)?;
    std::fs::copy(&found, dest_path).map_err(|e| {
        FbuildError::PackageError(format!(
            "failed to install probe-rs from {} to {}: {}",
            found.display(),
            dest_path.display(),
            e
        ))
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(dest_path)?.permissions();
        perms.set_mode(perms.mode() | 0o755);
        std::fs::set_permissions(dest_path, perms)?;
    }

    Ok(NormalizedPath::new(dest_path))
}

fn find_extracted_probe_rs_binary(root: &Path) -> Result<NormalizedPath> {
    let exe = if cfg!(windows) {
        "probe-rs.exe"
    } else {
        "probe-rs"
    };
    find_file_by_name(root, exe).ok_or_else(|| {
        FbuildError::PackageError(format!(
            "probe-rs binary `{exe}` not found after extracting {}",
            root.display()
        ))
    })
}

fn find_file_by_name(root: &Path, file_name: &str) -> Option<NormalizedPath> {
    let entries = std::fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file()
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name == file_name)
        {
            return Some(NormalizedPath::from(path));
        }
        if path.is_dir() {
            if let Some(found) = find_file_by_name(&path, file_name) {
                return Some(found);
            }
        }
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

fn home_dir_local() -> Option<NormalizedPath> {
    #[cfg(windows)]
    {
        if let Some(v) = std::env::var_os("USERPROFILE") {
            return Some(NormalizedPath::new(Path::new(&v)));
        }
    }
    std::env::var_os("HOME").map(|value| NormalizedPath::new(Path::new(&value)))
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

    #[test]
    fn release_asset_for_host_has_pinned_checksum_and_url() {
        let asset = probe_rs_release_asset_for_host().unwrap();
        assert!(asset
            .name
            .starts_with("probe-rs-fastled-fastled-v0.31.2-nusb-v1-transport-"));
        assert_eq!(asset.sha256.len(), 64);
        assert!(asset.sha256.chars().all(|c| c.is_ascii_hexdigit()));

        let url = asset.url();
        assert!(url.contains(PROBE_RS_RELEASE_TAG));
        assert!(url.ends_with(asset.name));
    }
}
