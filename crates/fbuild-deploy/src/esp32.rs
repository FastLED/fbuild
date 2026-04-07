//! ESP32 deployer using esptool.py.
//!
//! Flashes firmware to ESP32 boards via serial port using esptool.
//! Bootloader offset varies by MCU:
//! - `0x1000`: esp32, esp32s2
//! - `0x0`: esp32c2, esp32c3, esp32c5, esp32c6, esp32h2, esp32s3
//! - `0x2000`: esp32p4

use std::path::Path;

use fbuild_core::subprocess::run_command;
use fbuild_core::Result;

use crate::{Deployer, DeploymentResult};

/// Esptool flash parameters sourced from MCU config JSON.
///
/// All fields correspond to `esptool` section fields in the MCU config.
pub struct EsptoolParams {
    pub flash_mode: String,
    pub flash_freq: String,
    pub default_baud: String,
    pub before_reset: String,
    pub after_reset: String,
}

/// ESP32 deployer using `esptool`.
pub struct Esp32Deployer {
    /// MCU chip type for esptool --chip flag (e.g. "esp32c6").
    chip: String,
    /// Baud rate for flashing (e.g. "460800").
    baud_rate: String,
    /// Flash offsets.
    bootloader_offset: String,
    partitions_offset: String,
    firmware_offset: String,
    /// Flash mode for esptool (e.g. "dio", "qio").
    flash_mode: String,
    /// Flash frequency for esptool (e.g. "80m", "40m").
    flash_freq: String,
    /// Reset mode before flashing.
    before_reset: String,
    /// Reset mode after flashing.
    after_reset: String,
    verbose: bool,
}

impl Esp32Deployer {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        chip: &str,
        baud_rate: &str,
        bootloader_offset: &str,
        partitions_offset: &str,
        firmware_offset: &str,
        esptool_params: &EsptoolParams,
        verbose: bool,
    ) -> Self {
        Self {
            chip: chip.to_string(),
            baud_rate: baud_rate.to_string(),
            bootloader_offset: bootloader_offset.to_string(),
            partitions_offset: partitions_offset.to_string(),
            firmware_offset: firmware_offset.to_string(),
            flash_mode: esptool_params.flash_mode.clone(),
            flash_freq: esptool_params.flash_freq.clone(),
            before_reset: esptool_params.before_reset.clone(),
            after_reset: esptool_params.after_reset.clone(),
            verbose,
        }
    }

    /// Create an ESP32 deployer from board config with explicit flash offsets.
    pub fn from_board_config(
        board: &fbuild_config::BoardConfig,
        bootloader_offset: &str,
        partitions_offset: &str,
        firmware_offset: &str,
        esptool_params: &EsptoolParams,
        verbose: bool,
    ) -> Self {
        let baud = board
            .upload_speed
            .as_deref()
            .unwrap_or(&esptool_params.default_baud);
        // Board-level flash_mode overrides MCU default.
        let flash_mode = board
            .flash_mode
            .as_deref()
            .unwrap_or(&esptool_params.flash_mode);
        let params = EsptoolParams {
            flash_mode: flash_mode.to_string(),
            flash_freq: esptool_params.flash_freq.clone(),
            default_baud: esptool_params.default_baud.clone(),
            before_reset: esptool_params.before_reset.clone(),
            after_reset: esptool_params.after_reset.clone(),
        };
        Self::new(
            &board.mcu,
            baud,
            bootloader_offset,
            partitions_offset,
            firmware_offset,
            &params,
            verbose,
        )
    }

    /// Override the baud rate (e.g. from a CLI `--baud` flag).
    pub fn with_baud_rate(mut self, baud: &str) -> Self {
        self.baud_rate = baud.to_string();
        self
    }

    /// Find the esptool executable.
    ///
    /// Uses standalone `esptool` command (available when esptool is pip-installed).
    fn find_esptool() -> Vec<String> {
        vec!["esptool".to_string()]
    }

    /// Build the `esptool verify-flash` command line that this deployer
    /// would run to verify a candidate firmware image is already on the
    /// device. Pure (no I/O) so we can unit-test the argument layout
    /// without touching real hardware.
    ///
    /// `verify-flash` uses the device-side `FLASH_MD5SUM` command (issued
    /// by the stub flasher), so it does NOT read the entire flash region
    /// back over UART — verification of a 2.4 MB ESP32-S3 image takes
    /// ~6 seconds end-to-end vs ~25 seconds for a full re-flash.
    /// See ISSUES.md "Fast deploy via verify-then-skip".
    pub fn build_verify_flash_args(&self, firmware_path: &Path, port: &str) -> Vec<String> {
        let build_dir = firmware_path.parent().unwrap_or_else(|| Path::new("."));
        let bootloader_path = build_dir.join("bootloader.bin");
        let partitions_path = build_dir.join("partitions.bin");

        let mut args = Self::find_esptool();
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
        ]);

        // Verify all three regions in a single esptool invocation so we
        // pay the stub-flasher upload cost (~3s) once, not three times.
        if bootloader_path.exists() {
            args.push(self.bootloader_offset.clone());
            args.push(bootloader_path.to_string_lossy().to_string());
        }
        if partitions_path.exists() {
            args.push(self.partitions_offset.clone());
            args.push(partitions_path.to_string_lossy().to_string());
        }
        args.push(self.firmware_offset.clone());
        args.push(firmware_path.to_string_lossy().to_string());
        args
    }

    /// Run `esptool verify-flash` on bootloader + partitions + firmware
    /// against the live device. Returns `Ok(true)` when every region's
    /// FLASH_MD5SUM matches the local file (i.e. flashing would be a
    /// no-op), `Ok(false)` when at least one region differs, and `Err`
    /// only when esptool itself failed to run (port not found, stub
    /// upload failed, etc.).
    ///
    /// On success the chip is hard-reset by esptool's `--after hard_reset`,
    /// matching the post-flash behavior — so callers can treat a `true`
    /// return as "device is now running the requested firmware" without
    /// any extra reset.
    pub fn try_verify_deployment(&self, firmware_path: &Path, port: &str) -> Result<VerifyOutcome> {
        let args = self.build_verify_flash_args(firmware_path, port);
        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

        if self.verbose {
            tracing::info!("verify: {}", args.join(" "));
        }
        tracing::info!(
            "verifying {} on {} via esptool ({})",
            firmware_path.display(),
            port,
            self.chip
        );

        let result = run_command(
            &args_ref,
            None,
            None,
            // Verify is bounded: the slowest case is ~10s for a maximum
            // image. 30s gives plenty of headroom for slow USB-CDC stacks
            // and stub flasher upload retries.
            Some(std::time::Duration::from_secs(30)),
        )?;

        if result.success() {
            Ok(VerifyOutcome::Match {
                stdout: result.stdout,
                stderr: result.stderr,
            })
        } else {
            // esptool exits non-zero on a digest mismatch *and* on real
            // failures (port unreachable, stub upload error). Distinguish
            // them by looking at stderr — a true digest mismatch always
            // contains the literal "Verification failed" string.
            let combined = format!("{}\n{}", result.stdout, result.stderr);
            if combined.contains("Verification failed") || combined.contains("digest mismatch") {
                Ok(VerifyOutcome::Mismatch {
                    stdout: result.stdout,
                    stderr: result.stderr,
                })
            } else {
                Err(fbuild_core::FbuildError::DeployFailed(format!(
                    "esptool verify-flash failed (exit {}): {}",
                    result.exit_code, result.stderr
                )))
            }
        }
    }
}

/// Result of a `try_verify_deployment` call.
#[derive(Debug, Clone)]
pub enum VerifyOutcome {
    /// All flashed regions match the candidate image; flashing would be
    /// a no-op. The device has been hard-reset by esptool's
    /// `--after hard_reset` so it's already running the requested image.
    Match { stdout: String, stderr: String },
    /// At least one region differs from the local files; the caller
    /// should proceed with a normal `deploy()`.
    Mismatch { stdout: String, stderr: String },
}

impl VerifyOutcome {
    /// Convenience: returns `true` only for `Match`.
    pub fn is_match(&self) -> bool {
        matches!(self, VerifyOutcome::Match { .. })
    }
}

impl Deployer for Esp32Deployer {
    fn deploy(
        &self,
        project_dir: &Path,
        _env_name: &str,
        firmware_path: &Path,
        port: Option<&str>,
    ) -> Result<DeploymentResult> {
        let port = port.ok_or_else(|| {
            fbuild_core::FbuildError::DeployFailed(
                "serial port required for ESP32 deploy (use --port)".to_string(),
            )
        })?;

        let build_dir = firmware_path.parent().unwrap_or(project_dir);
        let bootloader_path = build_dir.join("bootloader.bin");
        let partitions_path = build_dir.join("partitions.bin");

        let mut args = Self::find_esptool();

        // Chip and port
        args.extend([
            "--chip".to_string(),
            self.chip.clone(),
            "--port".to_string(),
            port.to_string(),
            "--baud".to_string(),
            self.baud_rate.clone(),
        ]);

        // Reset behavior
        args.extend([
            "--before".to_string(),
            self.before_reset.clone(),
            "--after".to_string(),
            self.after_reset.clone(),
        ]);

        // Write flash command
        args.extend([
            "write_flash".to_string(),
            "-z".to_string(),
            "--flash-mode".to_string(),
            self.flash_mode.clone(),
            "--flash-freq".to_string(),
            self.flash_freq.clone(),
            "--flash-size".to_string(),
            "detect".to_string(),
        ]);

        // Flash addresses and files
        if bootloader_path.exists() {
            args.push(self.bootloader_offset.clone());
            args.push(bootloader_path.to_string_lossy().to_string());
        }

        if partitions_path.exists() {
            args.push(self.partitions_offset.clone());
            args.push(partitions_path.to_string_lossy().to_string());
        }

        args.push(self.firmware_offset.clone());
        args.push(firmware_path.to_string_lossy().to_string());

        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

        if self.verbose {
            tracing::info!("deploy: {}", args.join(" "));
        }

        tracing::info!(
            "flashing {} to {} via esptool ({})",
            firmware_path.display(),
            port,
            self.chip
        );

        let result = run_command(
            &args_ref,
            None,
            None,
            Some(std::time::Duration::from_secs(120)),
        )?;

        if result.success() {
            Ok(DeploymentResult {
                success: true,
                message: format!("firmware flashed to {} ({})", port, self.chip),
                port: Some(port.to_string()),
                stdout: result.stdout,
                stderr: result.stderr,
            })
        } else {
            // Return a non-success DeploymentResult instead of Err so the
            // daemon handler can forward esptool's stdout/stderr to the client.
            Ok(DeploymentResult {
                success: false,
                message: format!("esptool failed (exit code {})", result.exit_code),
                port: Some(port.to_string()),
                stdout: result.stdout,
                stderr: result.stderr,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test params matching ESP32-C6 JSON config values.
    fn test_esptool_params() -> EsptoolParams {
        EsptoolParams {
            flash_mode: "dio".to_string(),
            flash_freq: "80m".to_string(),
            default_baud: "460800".to_string(),
            before_reset: "default_reset".to_string(),
            after_reset: "hard_reset".to_string(),
        }
    }

    #[test]
    fn test_esp32_deployer_creation() {
        let params = test_esptool_params();
        let deployer = Esp32Deployer::new(
            "esp32c6", "460800", "0x0", "0x8000", "0x10000", &params, false,
        );
        assert_eq!(deployer.chip, "esp32c6");
        assert_eq!(deployer.baud_rate, "460800");
        assert_eq!(deployer.bootloader_offset, "0x0");
        assert_eq!(deployer.firmware_offset, "0x10000");
        assert_eq!(deployer.flash_mode, "dio");
        assert_eq!(deployer.before_reset, "default_reset");
    }

    #[test]
    fn test_esp32_deployer_from_board_config() {
        let board =
            fbuild_config::BoardConfig::from_board_id("esp32c6", &std::collections::HashMap::new())
                .unwrap();
        let params = test_esptool_params();
        let deployer =
            Esp32Deployer::from_board_config(&board, "0x0", "0x8000", "0x10000", &params, false);
        assert_eq!(deployer.chip, "esp32c6");
        assert_eq!(deployer.bootloader_offset, "0x0");
    }

    #[test]
    fn test_deploy_requires_port() {
        let params = test_esptool_params();
        let deployer = Esp32Deployer::new(
            "esp32c6", "460800", "0x0", "0x8000", "0x10000", &params, false,
        );
        let tmp = tempfile::TempDir::new().unwrap();
        let result = deployer.deploy(tmp.path(), "esp32c6", Path::new("firmware.bin"), None);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("serial port required"));
    }

    /// Fast deploy: the verify-flash command line must include the
    /// `verify-flash` subcommand and pair every flash region with its
    /// matching offset, in the order bootloader, partitions, firmware.
    /// Verifying all three in a single esptool call amortises the
    /// ~3-second stub flasher upload.
    #[test]
    fn build_verify_flash_args_includes_all_three_regions_when_present() {
        let params = test_esptool_params();
        let deployer = Esp32Deployer::new(
            "esp32s3", "921600", "0x0", "0x8000", "0x10000", &params, false,
        );
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("bootloader.bin"), b"boot").unwrap();
        std::fs::write(tmp.path().join("partitions.bin"), b"part").unwrap();
        let fw = tmp.path().join("firmware.bin");
        std::fs::write(&fw, b"firm").unwrap();

        let args = deployer.build_verify_flash_args(&fw, "COM13");

        // Subcommand
        assert!(
            args.contains(&"verify-flash".to_string()),
            "missing verify-flash subcommand: {:?}",
            args
        );
        // Chip + port
        assert!(args.contains(&"--chip".to_string()));
        assert!(args.contains(&"esp32s3".to_string()));
        assert!(args.contains(&"--port".to_string()));
        assert!(args.contains(&"COM13".to_string()));

        // All three (offset, file) pairs in the right order.
        let pos_verify = args.iter().position(|a| a == "verify-flash").unwrap();
        let pos_boot = args.iter().position(|a| a == "0x0").unwrap();
        let pos_parts = args.iter().position(|a| a == "0x8000").unwrap();
        let pos_fw = args.iter().position(|a| a == "0x10000").unwrap();
        assert!(
            pos_verify < pos_boot && pos_boot < pos_parts && pos_parts < pos_fw,
            "regions must appear after verify-flash in bootloader→partitions→firmware order: {:?}",
            args
        );

        // verify-flash MUST NOT carry --flash-mode/freq/size flags;
        // those are write-flash options and esptool 5.x rejects them
        // here. We were burned by this when we copied the deploy()
        // argument layout wholesale.
        assert!(
            !args.contains(&"--flash-mode".to_string()),
            "verify-flash must not include --flash-mode (write-flash only): {:?}",
            args
        );
        assert!(
            !args.contains(&"--flash-freq".to_string()),
            "verify-flash must not include --flash-freq (write-flash only): {:?}",
            args
        );
    }

    #[test]
    fn build_verify_flash_args_skips_missing_bootloader_and_partitions() {
        // When bootloader.bin / partitions.bin haven't been built (e.g.
        // an upload-only test fixture), verify must still cover firmware
        // alone. Otherwise we'd skip the only thing we have.
        let params = test_esptool_params();
        let deployer = Esp32Deployer::new(
            "esp32s3", "921600", "0x0", "0x8000", "0x10000", &params, false,
        );
        let tmp = tempfile::TempDir::new().unwrap();
        let fw = tmp.path().join("firmware.bin");
        std::fs::write(&fw, b"firm").unwrap();

        let args = deployer.build_verify_flash_args(&fw, "COM13");

        // No bootloader or partitions paths in the args.
        assert!(!args.iter().any(|a| a.ends_with("bootloader.bin")));
        assert!(!args.iter().any(|a| a.ends_with("partitions.bin")));
        // Firmware offset is still present.
        assert!(args.contains(&"0x10000".to_string()));
        assert!(args.iter().any(|a| a.ends_with("firmware.bin")));
    }

    #[test]
    fn verify_outcome_is_match_helper() {
        let m = VerifyOutcome::Match {
            stdout: "ok".into(),
            stderr: String::new(),
        };
        let mm = VerifyOutcome::Mismatch {
            stdout: String::new(),
            stderr: "Verification failed".into(),
        };
        assert!(m.is_match());
        assert!(!mm.is_match());
    }

    /// Hardware-gated integration test for fast deploy.
    ///
    /// Requires a real ESP32-S3 attached to `COM13` that has been
    /// pre-flashed with the FastLED reference firmware at
    /// `C:\Users\niteris\dev\fastled\.pio\build\esp32s3\firmware.bin`.
    /// Run with:
    /// ```
    /// uv run cargo test -p fbuild-deploy esp32::tests::try_verify_deployment_real_esp32s3 -- --ignored --nocapture
    /// ```
    ///
    /// Asserts (a) verify against the same image returns `Match` in
    /// under 15 seconds, and (b) verify against a tampered firmware
    /// (1 byte flipped) returns `Mismatch`.
    #[test]
    #[ignore = "requires ESP32-S3 attached to COM13 with FastLED reference firmware"]
    fn try_verify_deployment_real_esp32s3() {
        let port = "COM13";
        let reference: std::path::PathBuf =
            r"C:\Users\niteris\dev\fastled\.pio\build\esp32s3\firmware.bin".into();
        if !reference.exists() {
            panic!(
                "reference firmware not found at {}; pre-build FastLED's esp32s3 env first",
                reference.display()
            );
        }

        let params = EsptoolParams {
            flash_mode: "dio".to_string(),
            flash_freq: "80m".to_string(),
            default_baud: "921600".to_string(),
            before_reset: "default-reset".to_string(),
            after_reset: "hard-reset".to_string(),
        };
        let deployer = Esp32Deployer::new(
            "esp32s3", "921600", "0x0", "0x8000", "0x10000", &params, true,
        );

        // Phase 1: matching image → Match
        let start = std::time::Instant::now();
        let outcome = deployer
            .try_verify_deployment(&reference, port)
            .expect("verify must not fail to run against attached ESP32-S3");
        let elapsed = start.elapsed();
        assert!(
            outcome.is_match(),
            "expected Match against pre-flashed firmware; got {:?}",
            outcome
        );
        assert!(
            elapsed < std::time::Duration::from_secs(15),
            "verify took {:?} — should complete in <15s for the FastLED 2.4MB image",
            elapsed
        );
        eprintln!("verify (Match) elapsed: {:?}", elapsed);

        // Phase 2: tampered image → Mismatch
        let tmp = tempfile::TempDir::new().unwrap();
        // Copy bootloader and partitions next to the tampered firmware so
        // build_verify_flash_args picks them up alongside firmware.bin.
        let ref_dir = reference.parent().unwrap();
        std::fs::copy(
            ref_dir.join("bootloader.bin"),
            tmp.path().join("bootloader.bin"),
        )
        .unwrap();
        std::fs::copy(
            ref_dir.join("partitions.bin"),
            tmp.path().join("partitions.bin"),
        )
        .unwrap();
        let tampered = tmp.path().join("firmware.bin");
        let mut bytes = std::fs::read(&reference).unwrap();
        // Flip a byte well past the image header to avoid invalidating
        // the ESP-IDF magic and triggering an esptool parse error rather
        // than a clean digest mismatch.
        let target = bytes.len() / 2;
        bytes[target] ^= 0x55;
        std::fs::write(&tampered, &bytes).unwrap();

        let outcome = deployer
            .try_verify_deployment(&tampered, port)
            .expect("verify must not fail to run with tampered firmware");
        assert!(
            !outcome.is_match(),
            "expected Mismatch for tampered firmware; got {:?}",
            outcome
        );
        eprintln!("verify (Mismatch) detected correctly");
    }
}
