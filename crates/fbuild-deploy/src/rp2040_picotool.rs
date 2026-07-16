//! Managed picotool fallback for hosts whose synthetic UF2 volume rejects writes.

use std::path::Path;
use std::time::Duration;

use fbuild_core::{FbuildError, Result};
use fbuild_packages::Package;

pub(super) struct PicotoolLoad {
    pub stdout: String,
    pub stderr: String,
}

pub(super) async fn load_with_managed_picotool(
    project_dir: &Path,
    artifact: &Path,
    mass_storage_error: &str,
) -> Result<PicotoolLoad> {
    let package = fbuild_packages::toolchain::Rp2040Picotool::new(project_dir);
    Package::ensure_installed(&package).await?;
    let executable = package.executable();
    let args = load_args(&executable, artifact);
    let args_ref: Vec<&str> = args.iter().map(String::as_str).collect();
    let output =
        fbuild_core::subprocess::run_command(&args_ref, None, None, Some(Duration::from_secs(30)))
            .await?;
    if !output.success() {
        return Err(FbuildError::DeployFailed(format_failure(
            mass_storage_error,
            output.stdout.trim(),
            output.stderr.trim(),
            cfg!(windows),
        )));
    }
    Ok(PicotoolLoad {
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

fn load_args(executable: &Path, artifact: &Path) -> Vec<String> {
    vec![
        executable.to_string_lossy().to_string(),
        "load".to_string(),
        artifact.to_string_lossy().to_string(),
        "-x".to_string(),
    ]
}

fn format_failure(mass_storage_error: &str, stdout: &str, stderr: &str, windows: bool) -> String {
    let tool_output = [stderr, stdout]
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let host_hint = if windows {
        " On Windows, close software that scans removable drives or bind WinUSB to RP2 Boot (Interface 1), as documented by Raspberry Pi; this changes only the host driver and does not pre-flash the board."
    } else {
        " Check host USB permissions for the RP-series BOOTSEL interface."
    };
    format!(
        "RP-series deployment failed through both stock transports. Mass-storage error: {mass_storage_error}. Managed picotool error: {tool_output}.{host_hint}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_uses_managed_executable_and_reboots_after_success() {
        let args = load_args(Path::new("managed/picotool"), Path::new("firmware.uf2"));
        assert_eq!(args, ["managed/picotool", "load", "firmware.uf2", "-x"]);
    }

    #[test]
    fn combined_failure_preserves_both_transport_diagnostics() {
        let windows = format_failure("volume dirty", "", "driver unavailable", true);
        assert!(windows.contains("volume dirty"));
        assert!(windows.contains("driver unavailable"));
        assert!(windows.contains("does not pre-flash the board"));

        let unix = format_failure("volume dirty", "", "driver unavailable", false);
        assert!(unix.contains("volume dirty"));
        assert!(unix.contains("driver unavailable"));
        assert!(unix.contains("Check host USB permissions"));
        assert!(!unix.contains("does not pre-flash the board"));
    }
}
