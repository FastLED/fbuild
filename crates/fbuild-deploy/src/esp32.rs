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

/// ESP32 deployer using `python -m esptool` or `esptool.py`.
pub struct Esp32Deployer {
    /// MCU chip type for esptool --chip flag (e.g. "esp32c6").
    chip: String,
    /// Baud rate for flashing (e.g. "460800").
    baud_rate: String,
    /// Flash offsets.
    bootloader_offset: String,
    partitions_offset: String,
    firmware_offset: String,
    verbose: bool,
}

impl Esp32Deployer {
    pub fn new(
        chip: &str,
        baud_rate: &str,
        bootloader_offset: &str,
        partitions_offset: &str,
        firmware_offset: &str,
        verbose: bool,
    ) -> Self {
        Self {
            chip: chip.to_string(),
            baud_rate: baud_rate.to_string(),
            bootloader_offset: bootloader_offset.to_string(),
            partitions_offset: partitions_offset.to_string(),
            firmware_offset: firmware_offset.to_string(),
            verbose,
        }
    }

    /// Create an ESP32 deployer from board config with explicit flash offsets.
    pub fn from_board_config(
        board: &fbuild_config::BoardConfig,
        bootloader_offset: &str,
        partitions_offset: &str,
        firmware_offset: &str,
        verbose: bool,
    ) -> Self {
        Self::new(
            &board.mcu,
            board.upload_speed.as_deref().unwrap_or("460800"),
            bootloader_offset,
            partitions_offset,
            firmware_offset,
            verbose,
        )
    }

    /// Find the esptool executable.
    ///
    /// Uses `python -m esptool` (most reliable across platforms).
    fn find_esptool() -> Vec<String> {
        vec![
            "python".to_string(),
            "-m".to_string(),
            "esptool".to_string(),
        ]
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
            "default_reset".to_string(),
            "--after".to_string(),
            "hard_reset".to_string(),
        ]);

        // Write flash command
        args.extend([
            "write_flash".to_string(),
            "-z".to_string(),
            "--flash-mode".to_string(),
            "dio".to_string(),
            "--flash-freq".to_string(),
            "80m".to_string(),
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
            })
        } else {
            Err(fbuild_core::FbuildError::DeployFailed(format!(
                "esptool failed:\n{}",
                result.stderr
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_esp32_deployer_creation() {
        let deployer = Esp32Deployer::new("esp32c6", "460800", "0x0", "0x8000", "0x10000", false);
        assert_eq!(deployer.chip, "esp32c6");
        assert_eq!(deployer.baud_rate, "460800");
        assert_eq!(deployer.bootloader_offset, "0x0");
        assert_eq!(deployer.firmware_offset, "0x10000");
    }

    #[test]
    fn test_esp32_deployer_from_board_config() {
        let board =
            fbuild_config::BoardConfig::from_board_id("esp32c6", &std::collections::HashMap::new())
                .unwrap();
        let deployer = Esp32Deployer::from_board_config(&board, "0x0", "0x8000", "0x10000", false);
        assert_eq!(deployer.chip, "esp32c6");
        assert_eq!(deployer.bootloader_offset, "0x0");
    }

    #[test]
    fn test_deploy_requires_port() {
        let deployer = Esp32Deployer::new("esp32c6", "460800", "0x0", "0x8000", "0x10000", false);
        let tmp = tempfile::TempDir::new().unwrap();
        let result = deployer.deploy(tmp.path(), "esp32c6", Path::new("firmware.bin"), None);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("serial port required"));
    }
}
