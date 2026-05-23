//! `Esp32Deployer` core: construction, args, verify and write paths, and
//! the [`Deployer`] trait implementation.

use std::path::Path;

use fbuild_core::subprocess::run_command;
use fbuild_core::Result;

#[cfg(feature = "espflash-native")]
use super::parse::parse_hex_offset_u32;
use super::verify::{parse_verify_regions, FlashRegion, VerifyOutcome};
use crate::{DeployOutcome, Deployer, DeploymentResult};

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
    pub(super) chip: String,
    /// Baud rate for flashing (e.g. "460800").
    pub(super) baud_rate: String,
    /// Flash offsets.
    pub(super) bootloader_offset: String,
    pub(super) partitions_offset: String,
    pub(super) firmware_offset: String,
    /// Flash mode for esptool (e.g. "dio", "qio").
    pub(super) flash_mode: String,
    /// Flash frequency for esptool (e.g. "80m", "40m").
    pub(super) flash_freq: String,
    /// Reset mode before flashing.
    pub(super) before_reset: String,
    /// Reset mode after flashing.
    pub(super) after_reset: String,
    pub(super) verbose: bool,
    /// Route `verify-flash` through the native [`espflash`] crate
    /// instead of the Python `esptool` subprocess.
    ///
    /// The daemon sets this from the `FBUILD_USE_ESPFLASH_VERIFY` env
    /// var. When enabled, the deployer still falls back to esptool if
    /// the native path fails on a given board or host.
    ///
    /// Only compiled in when the `espflash-native` cargo feature is
    /// enabled.
    #[cfg(feature = "espflash-native")]
    pub(super) use_native_verify: bool,
    /// Route `write-flash` through the native [`espflash`] crate
    /// instead of the Python `esptool` subprocess (issue #66).
    ///
    /// The daemon sets this from the `FBUILD_USE_ESPFLASH_WRITE` env
    /// var, independently of `use_native_verify`. When enabled, the
    /// deployer still falls back to esptool if the native path fails.
    ///
    /// Feature-gated — see `use_native_verify`.
    #[cfg(feature = "espflash-native")]
    pub(super) use_native_write: bool,
}

#[cfg(feature = "espflash-native")]
pub(super) fn native_write_or_fallback<F>(
    port: &str,
    label: &str,
    native: F,
) -> Option<DeploymentResult>
where
    F: FnOnce() -> Result<DeploymentResult>,
{
    match native() {
        Ok(result) if result.success => Some(result),
        Ok(result) => {
            tracing::warn!(
                port,
                "native {} failed ({}); falling back to esptool",
                label,
                result.message
            );
            None
        }
        Err(e) => {
            tracing::warn!(
                port,
                "native {} failed ({}); falling back to esptool",
                label,
                e
            );
            None
        }
    }
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
            #[cfg(feature = "espflash-native")]
            use_native_verify: false,
            #[cfg(feature = "espflash-native")]
            use_native_write: false,
        }
    }

    /// Opt this deployer into the native espflash-based verify path
    /// (issue #66). Independent of `with_native_write`.
    ///
    /// Only present when the `espflash-native` cargo feature is enabled;
    /// without it the esptool-subprocess path is the only code path.
    #[cfg(feature = "espflash-native")]
    #[must_use]
    pub fn with_native_verify(mut self, enabled: bool) -> Self {
        self.use_native_verify = enabled;
        self
    }

    /// Opt this deployer into the native espflash-based write-flash
    /// path (issue #66). Independent of `with_native_verify`. When
    /// enabled, both [`Deployer::deploy`] and
    /// [`Esp32Deployer::deploy_regions`] route through the in-process
    /// espflash `Flasher`, skipping the ~1.5 s Python/esptool startup
    /// per flash unless they need to fall back.
    ///
    /// Only present when the `espflash-native` cargo feature is enabled.
    #[cfg(feature = "espflash-native")]
    #[must_use]
    pub fn with_native_write(mut self, enabled: bool) -> Self {
        self.use_native_write = enabled;
        self
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
    pub(super) fn find_esptool() -> Vec<String> {
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
    /// On success the chip is hard-reset by esptool's `--after hard-reset`,
    /// matching the post-flash behavior — so callers can treat a `true`
    /// return as "device is now running the requested firmware" without
    /// any extra reset.
    pub fn try_verify_deployment(&self, firmware_path: &Path, port: &str) -> Result<VerifyOutcome> {
        #[cfg(feature = "espflash-native")]
        if self.use_native_verify {
            match self.try_verify_deployment_native(firmware_path, port) {
                Ok(outcome) => return Ok(outcome),
                Err(e) => {
                    tracing::warn!(
                        port,
                        "native verify-flash failed ({}); falling back to esptool",
                        e
                    );
                }
            }
        }

        self.try_verify_deployment_esptool(firmware_path, port)
    }

    fn try_verify_deployment_esptool(
        &self,
        firmware_path: &Path,
        port: &str,
    ) -> Result<VerifyOutcome> {
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
                let regions = parse_verify_regions(
                    &combined,
                    &self.bootloader_offset,
                    &self.partitions_offset,
                    &self.firmware_offset,
                );
                Ok(VerifyOutcome::Mismatch {
                    stdout: result.stdout,
                    stderr: result.stderr,
                    regions,
                })
            } else {
                Err(fbuild_core::FbuildError::DeployFailed(format!(
                    "esptool verify-flash failed (exit {}): {}",
                    result.exit_code, result.stderr
                )))
            }
        }
    }

    /// Native `verify-flash` via the [`espflash`] crate (issue #66).
    ///
    /// Saves the Python interpreter startup (~1 s) and subprocess spawn
    /// (~0.5 s) that the esptool path pays per invocation. Same three
    /// regions (bootloader / partitions / firmware) and same
    /// [`VerifyOutcome`] semantics as the esptool path, so callers can
    /// swap between the two behind the `use_native_verify` flag without
    /// any result-handling changes.
    #[cfg(feature = "espflash-native")]
    fn try_verify_deployment_native(
        &self,
        firmware_path: &Path,
        port: &str,
    ) -> Result<VerifyOutcome> {
        let baud: u32 = self.baud_rate.parse().map_err(|e| {
            fbuild_core::FbuildError::DeployFailed(format!(
                "native verify: invalid baud rate '{}': {}",
                self.baud_rate, e
            ))
        })?;
        let boot_off = parse_hex_offset_u32(&self.bootloader_offset)?;
        let parts_off = parse_hex_offset_u32(&self.partitions_offset)?;
        let fw_off = parse_hex_offset_u32(&self.firmware_offset)?;

        let regions = crate::esp32_native::collect_standard_regions(
            firmware_path,
            boot_off,
            parts_off,
            fw_off,
        );

        if self.verbose {
            tracing::info!(
                "native verify: chip={} port={} baud={} regions={}",
                self.chip,
                port,
                baud,
                regions.len()
            );
        }
        tracing::info!(
            "verifying {} on {} via espflash ({})",
            firmware_path.display(),
            port,
            self.chip
        );

        crate::esp32_native::try_verify_deployment_native(
            &self.chip,
            port,
            baud,
            &self.before_reset,
            &self.after_reset,
            &regions,
            boot_off,
            parts_off,
            fw_off,
        )
    }

    /// Native `write-flash` via the [`espflash`] crate (issue #66).
    ///
    /// Writes the full three-region set (bootloader + partitions +
    /// firmware where present). Saves the Python interpreter startup
    /// (~1 s) plus subprocess spawn (~0.5 s) vs the esptool path, and
    /// surfaces per-region progress via `tracing` (bridged into the
    /// daemon's existing log broadcaster). Same [`DeploymentResult`]
    /// shape as the esptool path so callers swap behind a single flag.
    #[cfg(feature = "espflash-native")]
    pub(super) fn try_deploy_native(
        &self,
        firmware_path: &Path,
        port: &str,
    ) -> Result<DeploymentResult> {
        let baud = self.parse_native_baud()?;
        let (boot_off, parts_off, fw_off) = self.parse_native_offsets()?;

        let regions = crate::esp32_native::collect_standard_write_regions(
            firmware_path,
            boot_off,
            parts_off,
            fw_off,
        );

        if self.verbose {
            tracing::info!(
                "native write: chip={} port={} baud={} regions={}",
                self.chip,
                port,
                baud,
                regions.len()
            );
        }
        tracing::info!(
            "flashing {} to {} via espflash ({})",
            firmware_path.display(),
            port,
            self.chip
        );

        crate::esp32_native::try_write_deployment_native(
            &self.chip,
            port,
            baud,
            &self.before_reset,
            &self.after_reset,
            &regions,
            /* selective */ false,
        )
    }

    /// Native `write-flash` for a caller-chosen subset of regions
    /// (issue #66). Used after a verify-mismatch to rewrite only the
    /// regions that actually differ — skipping the ~1s
    /// bootloader/partitions rewrite when only firmware changed.
    #[cfg(feature = "espflash-native")]
    pub(super) fn try_deploy_regions_native(
        &self,
        firmware_path: &Path,
        port: &str,
        regions: &[FlashRegion],
    ) -> Result<DeploymentResult> {
        let baud = self.parse_native_baud()?;
        let (boot_off, parts_off, fw_off) = self.parse_native_offsets()?;

        let write_regions = crate::esp32_native::collect_selected_write_regions(
            firmware_path,
            boot_off,
            parts_off,
            fw_off,
            regions,
        )?;

        if self.verbose {
            tracing::info!(
                "native write (selective): chip={} port={} baud={} regions={:?}",
                self.chip,
                port,
                baud,
                regions
            );
        }
        tracing::info!(
            "flashing regions {:?} of {} to {} via espflash ({})",
            regions,
            firmware_path.display(),
            port,
            self.chip
        );

        crate::esp32_native::try_write_deployment_native(
            &self.chip,
            port,
            baud,
            &self.before_reset,
            &self.after_reset,
            &write_regions,
            /* selective */ true,
        )
    }

    #[cfg(feature = "espflash-native")]
    fn parse_native_baud(&self) -> Result<u32> {
        self.baud_rate.parse().map_err(|e| {
            fbuild_core::FbuildError::DeployFailed(format!(
                "native write: invalid baud rate '{}': {}",
                self.baud_rate, e
            ))
        })
    }

    #[cfg(feature = "espflash-native")]
    fn parse_native_offsets(&self) -> Result<(u32, u32, u32)> {
        let boot = parse_hex_offset_u32(&self.bootloader_offset)?;
        let parts = parse_hex_offset_u32(&self.partitions_offset)?;
        let fw = parse_hex_offset_u32(&self.firmware_offset)?;
        Ok((boot, parts, fw))
    }
}

impl Esp32Deployer {
    /// Build `esptool write-flash` argv for a caller-chosen subset of
    /// regions. Pass `None` for `regions` to flash all three (bootloader +
    /// partitions + firmware) when present on disk — the default deploy
    /// path. Pass `Some(&[...])` to restrict the write to specific regions
    /// (see `deploy_regions`).
    pub fn build_write_flash_args(
        &self,
        firmware_path: &Path,
        port: &str,
        regions: Option<&[FlashRegion]>,
    ) -> Vec<String> {
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
            "write-flash".to_string(),
            "-z".to_string(),
            "--flash-mode".to_string(),
            self.flash_mode.clone(),
            "--flash-freq".to_string(),
            self.flash_freq.clone(),
            "--flash-size".to_string(),
            "detect".to_string(),
        ]);

        let include = |r: FlashRegion| regions.map_or(true, |rs| rs.contains(&r));

        if include(FlashRegion::Bootloader) && bootloader_path.exists() {
            args.push(self.bootloader_offset.clone());
            args.push(bootloader_path.to_string_lossy().to_string());
        }
        if include(FlashRegion::Partitions) && partitions_path.exists() {
            args.push(self.partitions_offset.clone());
            args.push(partitions_path.to_string_lossy().to_string());
        }
        if include(FlashRegion::Firmware) {
            args.push(self.firmware_offset.clone());
            args.push(firmware_path.to_string_lossy().to_string());
        }
        args
    }

    /// Flash only the specified regions. Use after `try_verify_deployment`
    /// returns a `Mismatch` with `regions` populated — we skip the ~1s
    /// bootloader/partitions rewrite when only firmware differs (the
    /// overwhelmingly common case for iterative development).
    ///
    /// Returns an error when `regions` is empty; esptool rejects a
    /// write-flash call with no offset/file pair and the message would be
    /// opaque.
    pub fn deploy_regions(
        &self,
        firmware_path: &Path,
        port: &str,
        regions: &[FlashRegion],
    ) -> Result<DeploymentResult> {
        if regions.is_empty() {
            return Err(fbuild_core::FbuildError::DeployFailed(
                "deploy_regions called with no regions; pass at least one".to_string(),
            ));
        }

        // Fail loudly if the caller asked for a region whose file isn't
        // on disk. Without this check `build_write_flash_args` would
        // silently drop the request and esptool would complain with an
        // opaque usage error.
        let build_dir = firmware_path.parent().unwrap_or_else(|| Path::new("."));
        for region in regions {
            let (name, path) = match region {
                FlashRegion::Bootloader => ("bootloader.bin", build_dir.join("bootloader.bin")),
                FlashRegion::Partitions => ("partitions.bin", build_dir.join("partitions.bin")),
                FlashRegion::Firmware => continue,
            };
            if !path.exists() {
                return Err(fbuild_core::FbuildError::DeployFailed(format!(
                    "deploy_regions requested {:?} but {} is missing from {}",
                    region,
                    name,
                    build_dir.display()
                )));
            }
        }

        #[cfg(feature = "espflash-native")]
        if self.use_native_write {
            if let Some(result) = native_write_or_fallback(port, "selective write-flash", || {
                self.try_deploy_regions_native(firmware_path, port, regions)
            }) {
                return Ok(result);
            }
        }

        let args = self.build_write_flash_args(firmware_path, port, Some(regions));
        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

        if self.verbose {
            tracing::info!("deploy (selective): {}", args.join(" "));
        }
        tracing::info!(
            "flashing regions {:?} of {} to {} via esptool ({})",
            regions,
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
                message: format!(
                    "{} region(s) flashed to {} ({})",
                    regions.len(),
                    port,
                    self.chip
                ),
                port: Some(port.to_string()),
                stdout: result.stdout,
                stderr: result.stderr,
                outcome: DeployOutcome::SelectiveFlash {
                    regions: regions.to_vec(),
                },
            })
        } else {
            Ok(DeploymentResult {
                success: false,
                message: format!("esptool failed (exit code {})", result.exit_code),
                port: Some(port.to_string()),
                stdout: result.stdout,
                stderr: result.stderr,
                outcome: DeployOutcome::SelectiveFlash {
                    regions: regions.to_vec(),
                },
            })
        }
    }
}

impl Deployer for Esp32Deployer {
    fn deploy(
        &self,
        _project_dir: &Path,
        _env_name: &str,
        firmware_path: &Path,
        port: Option<&str>,
    ) -> Result<DeploymentResult> {
        let port = port.ok_or_else(|| {
            fbuild_core::FbuildError::DeployFailed(
                "serial port required for ESP32 deploy (use --port)".to_string(),
            )
        })?;

        #[cfg(feature = "espflash-native")]
        if self.use_native_write {
            if let Some(result) = native_write_or_fallback(port, "write-flash", || {
                self.try_deploy_native(firmware_path, port)
            }) {
                return Ok(result);
            }
        }

        let args = self.build_write_flash_args(firmware_path, port, None);
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
                outcome: DeployOutcome::FullFlash,
            })
        } else {
            Ok(DeploymentResult {
                success: false,
                message: format!("esptool failed (exit code {})", result.exit_code),
                port: Some(port.to_string()),
                stdout: result.stdout,
                stderr: result.stderr,
                outcome: DeployOutcome::FullFlash,
            })
        }
    }
}
