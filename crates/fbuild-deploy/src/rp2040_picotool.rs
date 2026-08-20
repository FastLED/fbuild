//! Managed picotool transport: used as the primary RP2040 transport
//! (FastLED/fbuild#1162) or as the fallback for hosts whose synthetic UF2
//! volume rejects writes.

use std::path::Path;
use std::time::Duration;

use fbuild_core::{FbuildError, Result};
use fbuild_packages::Package;

/// BOOTSEL identity selected before fbuild invokes picotool. The runtime CDC
/// port is only a control path; the serial number and ROM PID bind the
/// subsequent PICOBOOT operation to the board that was touched.
#[derive(Debug)]
pub(super) struct PicotoolTarget {
    serial_number: String,
    vendor_id: String,
    product_id: String,
}

impl PicotoolTarget {
    pub(super) fn new(serial_number: &str, vendor_id: &str, product_id: &str) -> Self {
        Self {
            serial_number: serial_number.to_string(),
            vendor_id: vendor_id.to_string(),
            product_id: product_id.to_string(),
        }
    }

    pub(super) fn matches_usb_instance(&self, instance_id: &str) -> bool {
        let upper = instance_id.to_ascii_uppercase();
        upper.contains(&format!("VID_{}", self.vendor_id.to_ascii_uppercase()))
            && upper.contains(&format!("PID_{}", self.product_id.to_ascii_uppercase()))
    }
}

pub(super) struct PicotoolLoad {
    pub stdout: String,
    pub stderr: String,
}

/// Whether `load_with_managed_picotool` is running as the primary transport
/// (mass-storage has not been attempted yet) or as the fallback after
/// mass-storage already failed. Controls only failure-message wording: a
/// primary-mode failure is not yet a "both transports failed" situation —
/// the caller still has a mass-storage fallback to try — so it returns the
/// bare picotool error text instead of a combined message.
#[derive(Debug, Clone, Copy)]
pub(super) enum PicotoolMode {
    Primary,
    Fallback,
}

/// Direction a combined "both transports failed" message reads in
/// (FastLED/fbuild#1162): which transport ran first. Only used once both
/// transports are known to have failed.
#[derive(Debug, Clone, Copy)]
pub(super) enum FailureDirection {
    /// Mass-storage ran first (transport = `uf2`, or `--transport picotool`
    /// with picotool primary already used as the fallback path itself —
    /// historical order).
    PicotoolFallback,
    /// Picotool ran first (transport = `picotool`, the default); mass-storage
    /// was attempted only after picotool failed or was skipped by preflight.
    PicotoolPrimary,
}

#[derive(Debug)]
pub(super) enum PriorTransportFailure {
    PicotoolPrimary(String),
    MassStoragePrimary(String),
}

/// Bounded `picotool info` probe: proves the PICOBOOT vendor interface is
/// reachable before committing to the (longer) load timeout. Failure here is
/// classified as a transport/device failure by the caller, which falls back
/// to mass-storage.
pub(super) async fn probe_picotool_info(
    project_dir: &Path,
    target: &PicotoolTarget,
    timeout: Duration,
) -> Result<()> {
    let package = fbuild_packages::toolchain::Rp2040Picotool::new(project_dir);
    Package::ensure_installed(&package).await?;
    let executable = package.executable();
    let args = info_probe_args(&executable, target);
    let args_ref: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = fbuild_core::subprocess::run_command(&args_ref, None, None, Some(timeout)).await?;
    if !output.success() {
        return Err(FbuildError::DeployFailed(combined_tool_output(
            output.stdout.trim(),
            output.stderr.trim(),
        )));
    }
    Ok(())
}

/// Ask a cooperative runtime application to enter USB BOOTSEL. Unlike a
/// 1200-bps touch this uses the Pico SDK reset interface, so it still works
/// when the selected board's CDC endpoint cannot be opened. The caller must
/// supply the exact runtime VID/PID and USB serial. Windows callers use the
/// native WinUSB reset-interface path when it can be resolved exactly; this
/// libusb fallback remains target-filtered for other hosts.
pub(super) async fn reboot_runtime_to_bootsel(
    project_dir: &Path,
    target: &PicotoolTarget,
    timeout: Duration,
) -> Result<PicotoolLoad> {
    let package = fbuild_packages::toolchain::Rp2040Picotool::new(project_dir);
    Package::ensure_installed(&package).await?;
    let executable = package.executable();
    let args = reboot_to_bootsel_args(&executable, target);
    let args_ref: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = fbuild_core::subprocess::run_command(&args_ref, None, None, Some(timeout)).await?;
    if !output.success() {
        return Err(FbuildError::DeployFailed(format!(
            "managed picotool application-mode reboot error: {}",
            combined_tool_output(output.stdout.trim(), output.stderr.trim())
        )));
    }
    Ok(PicotoolLoad {
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

/// Ask the already-installed managed picotool for its most recent UF2
/// diagnostic. This must never trigger a package download; it shares the
/// caller's bounded subprocess timeout (FastLED/fbuild#1245).
pub(super) async fn probe_uf2_rejection_info(
    project_dir: &Path,
    timeout: Duration,
) -> Result<String> {
    let package = fbuild_packages::toolchain::Rp2040Picotool::new(project_dir);
    let executable = package.executable();
    if !executable.is_file() {
        return Err(FbuildError::DeployFailed(
            "managed picotool is not installed; skipping ROM UF2 diagnostic".to_string(),
        ));
    }
    let args = uf2_info_args(&executable);
    let args_ref: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = fbuild_core::subprocess::run_command(&args_ref, None, None, Some(timeout)).await?;
    let diagnostic = combined_tool_output(output.stdout.trim(), output.stderr.trim());
    if diagnostic.is_empty() {
        return Err(FbuildError::DeployFailed(
            "managed picotool uf2 info returned no diagnostic".to_string(),
        ));
    }
    Ok(diagnostic)
}

pub(super) async fn load_with_managed_picotool(
    project_dir: &Path,
    artifact: &Path,
    target: &PicotoolTarget,
    mass_storage_error: Option<&str>,
    timeout: Duration,
    mode: PicotoolMode,
) -> Result<PicotoolLoad> {
    let package = fbuild_packages::toolchain::Rp2040Picotool::new(project_dir);
    Package::ensure_installed(&package).await?;
    let executable = package.executable();
    let args = load_args(&executable, artifact, target);
    let args_ref: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = fbuild_core::subprocess::run_command(&args_ref, None, None, Some(timeout)).await?;
    if !output.success() {
        let tool_output = combined_tool_output(output.stdout.trim(), output.stderr.trim());
        let message = match mode {
            PicotoolMode::Fallback => format_failure(
                FailureDirection::PicotoolFallback,
                mass_storage_error.unwrap_or("unknown mass-storage error"),
                &tool_output,
                fbuild_core::platform::host::is_windows(),
            ),
            // Mass-storage has not run yet; the caller composes the final
            // combined message only if it also fails.
            PicotoolMode::Primary => format!("managed picotool load error: {tool_output}"),
        };
        return Err(FbuildError::DeployFailed(message));
    }
    Ok(PicotoolLoad {
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

fn combined_tool_output(stdout: &str, stderr: &str) -> String {
    [stderr, stdout]
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn append_target_selection(args: &mut Vec<String>, target: &PicotoolTarget) {
    append_target_vid_pid(args, target);
    args.extend(["--ser".to_string(), target.serial_number.to_string()]);
}

fn append_target_vid_pid(args: &mut Vec<String>, target: &PicotoolTarget) {
    args.extend([
        "--vid".to_string(),
        format!("0x{}", target.vendor_id),
        "--pid".to_string(),
        format!("0x{}", target.product_id),
    ]);
}

fn load_args(executable: &Path, artifact: &Path, target: &PicotoolTarget) -> Vec<String> {
    let mut args = vec![
        executable.to_string_lossy().to_string(),
        "load".to_string(),
        artifact.to_string_lossy().to_string(),
        "-x".to_string(),
    ];
    append_target_selection(&mut args, target);
    args
}

fn reboot_to_bootsel_args(executable: &Path, target: &PicotoolTarget) -> Vec<String> {
    let mut args = vec![
        executable.to_string_lossy().to_string(),
        "reboot".to_string(),
        "-u".to_string(),
    ];
    // pico-quick-toolchain's pinned picotool uses an order-sensitive
    // CLIPP grammar: reboot-type options precede device selectors, and `-f`
    // is the final option in the selector group. Keep the application-mode
    // VID/PID selectors. Do not pass
    // `--ser` here: current picotool applies it while opening the application
    // device, where Arduino-Pico's reset function does not expose the BOOTSEL
    // serial. When omitted, picotool reads the application device descriptor
    // and tracks that serial automatically across the reboot. fbuild has
    // already resolved one exact healthy reset interface, and picotool itself
    // refuses a forced command if the VID/PID matches multiple devices.
    append_target_vid_pid(&mut args, target);
    args.push("-f".to_string());
    args
}

fn info_probe_args(executable: &Path, target: &PicotoolTarget) -> Vec<String> {
    let mut args = vec![executable.to_string_lossy().to_string(), "info".to_string()];
    append_target_selection(&mut args, target);
    args
}

fn uf2_info_args(executable: &Path) -> Vec<String> {
    vec![
        executable.to_string_lossy().to_string(),
        "uf2".to_string(),
        "info".to_string(),
    ]
}

fn host_hint(windows: bool) -> &'static str {
    if windows {
        " On Windows, close software that scans removable drives or bind WinUSB to RP2 Boot (Interface 1), as documented by Raspberry Pi; this changes only the host driver and does not pre-flash the board."
    } else {
        " Check host USB permissions for the RP-series BOOTSEL interface."
    }
}

/// Direction-aware composition of the final "both RP-series transports
/// failed" message (FastLED/fbuild#1162). `picotool_error` may be a real
/// tool-output string (probe/load failure) or, for `PicotoolPrimary` when
/// preflight skipped picotool outright, the driver-missing diagnostic text —
/// either way it is named as picotool's failure reason.
pub(super) fn format_failure(
    direction: FailureDirection,
    mass_storage_error: &str,
    picotool_error: &str,
    windows: bool,
) -> String {
    match direction {
        FailureDirection::PicotoolFallback => format!(
            "RP-series deployment failed through both stock transports. Mass-storage error: {mass_storage_error}. Managed picotool error: {picotool_error}.{}",
            host_hint(windows)
        ),
        FailureDirection::PicotoolPrimary => format!(
            "RP-series deployment failed through both stock transports. Picotool error: {picotool_error}. Mass-storage fallback error: {mass_storage_error}.{}",
            host_hint(windows)
        ),
    }
}

pub(super) fn format_eject_failure(
    mass_storage_error: &str,
    prior_failure: Option<&PriorTransportFailure>,
    uf2_diagnostic: Option<&str>,
) -> String {
    let mut message = match prior_failure {
        Some(PriorTransportFailure::PicotoolPrimary(picotool_error)) => format_failure(
            FailureDirection::PicotoolPrimary,
            mass_storage_error,
            picotool_error,
            fbuild_core::platform::host::is_windows(),
        ),
        Some(PriorTransportFailure::MassStoragePrimary(prior_mass_storage_error)) => {
            format_failure(
                FailureDirection::PicotoolFallback,
                prior_mass_storage_error,
                mass_storage_error,
                fbuild_core::platform::host::is_windows(),
            )
        }
        None => mass_storage_error.to_string(),
    };
    if let Some(diagnostic) = uf2_diagnostic {
        message.push_str(" Managed picotool UF2 diagnostic: ");
        message.push_str(diagnostic);
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rp2350_target() -> PicotoolTarget {
        PicotoolTarget::new("2DCB876B587EA334", "2e8a", "000f")
    }

    #[test]
    fn load_is_bound_to_the_bootsel_target_and_reboots_after_success() {
        let args = load_args(
            Path::new("managed/picotool"),
            Path::new("firmware.uf2"),
            &rp2350_target(),
        );
        assert_eq!(
            args,
            [
                "managed/picotool",
                "load",
                "firmware.uf2",
                "-x",
                "--vid",
                "0x2e8a",
                "--pid",
                "0x000f",
                "--ser",
                "2DCB876B587EA334",
            ]
        );
    }

    #[test]
    fn forced_application_reboot_is_bound_to_the_runtime_target() {
        let target = PicotoolTarget::new("2DCB876B587EA334", "2e8a", "f00f");
        let args = reboot_to_bootsel_args(Path::new("managed/picotool"), &target);
        assert_eq!(
            args,
            [
                "managed/picotool",
                "reboot",
                "-u",
                "--vid",
                "0x2e8a",
                "--pid",
                "0xf00f",
                "-f",
            ]
        );
    }

    #[test]
    fn info_probe_uses_the_same_bootsel_target() {
        let args = info_probe_args(Path::new("managed/picotool"), &rp2350_target());
        assert_eq!(
            args,
            [
                "managed/picotool",
                "info",
                "--vid",
                "0x2e8a",
                "--pid",
                "0x000f",
                "--ser",
                "2DCB876B587EA334",
            ]
        );
    }

    #[test]
    fn uf2_rejection_probe_uses_managed_executable() {
        let args = uf2_info_args(Path::new("managed/picotool"));
        assert_eq!(args, ["managed/picotool", "uf2", "info"]);
    }

    #[test]
    fn eject_failure_preserves_prior_transport_and_uf2_diagnostics() {
        let message = format_eject_failure(
            "BOOTSEL volume did not eject",
            Some(&PriorTransportFailure::PicotoolPrimary(
                "picotool load failed at 0%".to_string(),
            )),
            Some("ERROR_INCOMPATIBLE_IMAGE"),
        );
        assert!(message.contains("Picotool error: picotool load failed at 0%"));
        assert!(message.contains("Mass-storage fallback error: BOOTSEL volume did not eject"));
        assert!(message.contains("Managed picotool UF2 diagnostic: ERROR_INCOMPATIBLE_IMAGE"));
    }

    #[test]
    fn eject_failure_preserves_mass_storage_first_ordering() {
        let message = format_eject_failure(
            "picotool post-load volume did not eject",
            Some(&PriorTransportFailure::MassStoragePrimary(
                "mass-storage write failed".to_string(),
            )),
            None,
        );
        assert!(message.contains("Mass-storage error: mass-storage write failed"));
        assert!(
            message.contains("Managed picotool error: picotool post-load volume did not eject")
        );
    }

    #[test]
    fn eject_failure_keeps_primary_error_when_rom_diagnostic_is_unavailable() {
        let message = format_eject_failure("volume did not eject", None, None);
        assert_eq!(message, "volume did not eject");
    }

    #[test]
    fn fallback_combined_failure_preserves_both_transport_diagnostics() {
        let windows = format_failure(
            FailureDirection::PicotoolFallback,
            "volume dirty",
            "driver unavailable",
            true,
        );
        assert!(windows.contains("volume dirty"));
        assert!(windows.contains("driver unavailable"));
        assert!(windows.contains("does not pre-flash the board"));
        assert!(windows.contains("Mass-storage error: volume dirty"));
        assert!(windows.contains("Managed picotool error: driver unavailable"));

        let unix = format_failure(
            FailureDirection::PicotoolFallback,
            "volume dirty",
            "driver unavailable",
            false,
        );
        assert!(unix.contains("volume dirty"));
        assert!(unix.contains("driver unavailable"));
        assert!(unix.contains("Check host USB permissions"));
        assert!(!unix.contains("does not pre-flash the board"));
    }

    #[test]
    fn primary_combined_failure_names_picotool_first() {
        let message = format_failure(
            FailureDirection::PicotoolPrimary,
            "volume dirty",
            "WinUSB binding missing",
            true,
        );
        assert!(message.contains("Picotool error: WinUSB binding missing"));
        assert!(message.contains("Mass-storage fallback error: volume dirty"));
        // Direction matters: "Picotool error" must precede "Mass-storage
        // fallback error" in the primary-direction message.
        let picotool_index = message.find("Picotool error").unwrap();
        let mass_storage_index = message.find("Mass-storage fallback error").unwrap();
        assert!(picotool_index < mass_storage_index);
    }
}
