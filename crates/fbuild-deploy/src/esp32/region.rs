//! Single-region `esptool` flash and verify (`fbuild uploadfs`, FastLED/fbuild#1358).
//!
//! A filesystem image is one file at one caller-chosen offset, so nothing is
//! inferred from a build directory (unlike the firmware deploy path in
//! `deployer`). Kept in its own module so `deployer` stays under the LOC gate.

use std::path::Path;

use fbuild_core::Result;
use fbuild_core::subprocess::run_command;

use super::deployer::Esp32Deployer;

impl Esp32Deployer {
    /// Build `esptool write-flash` argv for a single file at a caller-chosen
    /// flash offset — a filesystem image written to its partition
    /// (`fbuild uploadfs`, FastLED/fbuild#1358).
    ///
    /// Unlike [`Self::build_write_flash_args`] there is nothing to infer from
    /// a build directory here: a filesystem partition can sit at any offset
    /// the project's partition CSV declares, so the caller owns the offset.
    /// Pure (no I/O) so the layout stays unit-testable without hardware.
    pub fn build_flash_region_args(&self, image: &Path, port: &str, offset: &str) -> Vec<String> {
        let mut args = self.find_esptool();
        args.extend([
            "--chip".to_string(),
            self.chip.clone(),
            "--port".to_string(),
            port.to_string(),
            "--baud".to_string(),
            self.baud_rate.clone(),
            "--before".to_string(),
            self.before_reset.clone(),
            "--after".to_string(),
            self.after_reset.clone(),
            "write-flash".to_string(),
            "-z".to_string(),
            "--flash-mode".to_string(),
            self.flash_mode.clone(),
            "--flash-freq".to_string(),
            self.flash_freq.clone(),
            "--flash-size".to_string(),
            self.flash_size.clone(),
            offset.to_string(),
            image.to_string_lossy().to_string(),
        ]);
        args
    }

    /// Build `esptool verify-flash` argv for that same single region: the
    /// device-side MD5 comparison, so `uploadfs --verify` proves the bytes
    /// actually landed instead of only that esptool exited zero.
    pub fn build_verify_region_args(&self, image: &Path, port: &str, offset: &str) -> Vec<String> {
        let mut args = self.find_esptool();
        args.extend([
            "--chip".to_string(),
            self.chip.clone(),
            "--port".to_string(),
            port.to_string(),
            "--baud".to_string(),
            self.baud_rate.clone(),
            "--before".to_string(),
            self.before_reset.clone(),
            "--after".to_string(),
            self.after_reset.clone(),
            "verify-flash".to_string(),
            offset.to_string(),
            image.to_string_lossy().to_string(),
        ]);
        args
    }

    /// Write a single image to a single flash offset (`fbuild uploadfs`).
    ///
    /// Returns [`RegionFlashResult`] rather than [`DeploymentResult`]: this is
    /// one region of a project's flash, not a firmware deploy, so there is no
    /// [`DeployOutcome`] to report and no trusted-hash bookkeeping to update.
    pub async fn flash_region(
        &self,
        image: &Path,
        port: &str,
        offset: &str,
    ) -> Result<RegionFlashResult> {
        self.run_region_command(
            "write-flash",
            self.build_flash_region_args(image, port, offset),
        )
        .await
    }

    /// `esptool verify-flash` for a single region written by
    /// [`Self::flash_region`]. `success == false` with an `exit_code` of 0 is
    /// impossible; a mismatch surfaces as a non-zero esptool exit, whose
    /// stderr names the differing region.
    pub async fn verify_region(
        &self,
        image: &Path,
        port: &str,
        offset: &str,
    ) -> Result<RegionFlashResult> {
        self.run_region_command(
            "verify-flash",
            self.build_verify_region_args(image, port, offset),
        )
        .await
    }

    async fn run_region_command(
        &self,
        operation: &str,
        args: Vec<String>,
    ) -> Result<RegionFlashResult> {
        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        if self.verbose {
            tracing::info!("{}: {}", operation, args.join(" "));
        }
        let env_overlay =
            fbuild_core::subprocess::bare_name_path_overlay(&args[0], self.caller_path.as_deref());
        let result = run_command(
            &args_ref,
            None,
            env_overlay.as_deref(),
            Some(std::time::Duration::from_secs(180)),
        )
        .await?;
        Ok(RegionFlashResult {
            success: result.success(),
            exit_code: result.exit_code,
            stdout: result.stdout,
            stderr: result.stderr,
        })
    }
}

/// Result of a single-region `esptool` operation (`fbuild uploadfs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionFlashResult {
    pub success: bool,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}
