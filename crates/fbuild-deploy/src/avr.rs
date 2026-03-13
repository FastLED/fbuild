//! AVR deployer using avrdude.
//!
//! Flashes firmware.hex to Arduino boards via serial port.

use std::path::{Path, PathBuf};

use fbuild_core::subprocess::run_command;
use fbuild_core::Result;

use crate::{Deployer, DeploymentResult};

/// AVR deployer using avrdude.
pub struct AvrDeployer {
    /// Path to avrdude binary (if not in PATH).
    avrdude_path: PathBuf,
    /// MCU type for avrdude (-p flag), e.g. "atmega328p".
    mcu: String,
    /// Programmer type (-c flag), e.g. "arduino".
    programmer: String,
    /// Baud rate (-b flag), e.g. "115200".
    baud_rate: String,
    verbose: bool,
}

impl AvrDeployer {
    pub fn new(
        mcu: &str,
        programmer: &str,
        baud_rate: &str,
        avrdude_path: Option<PathBuf>,
        verbose: bool,
    ) -> Self {
        Self {
            avrdude_path: avrdude_path.unwrap_or_else(|| PathBuf::from("avrdude")),
            mcu: mcu.to_string(),
            programmer: programmer.to_string(),
            baud_rate: baud_rate.to_string(),
            verbose,
        }
    }

    /// Create an AVR deployer from board config defaults.
    pub fn from_board_config(board: &fbuild_config::BoardConfig, verbose: bool) -> Self {
        Self::new(
            &board.mcu,
            board.upload_protocol.as_deref().unwrap_or("arduino"),
            board.upload_speed.as_deref().unwrap_or("115200"),
            None,
            verbose,
        )
    }
}

impl Deployer for AvrDeployer {
    fn deploy(
        &self,
        _project_dir: &Path,
        _env_name: &str,
        firmware_path: &Path,
        port: Option<&str>,
    ) -> Result<DeploymentResult> {
        let port = port.ok_or_else(|| {
            fbuild_core::FbuildError::DeployFailed(
                "serial port required for AVR deploy (use --port)".to_string(),
            )
        })?;

        let flash_arg = format!("flash:w:{}:i", firmware_path.display());

        let args = [
            self.avrdude_path.to_string_lossy().to_string(),
            "-v".to_string(),
            format!("-p{}", self.mcu),
            format!("-c{}", self.programmer),
            format!("-P{}", port),
            format!("-b{}", self.baud_rate),
            format!("-U{}", flash_arg),
        ];

        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

        if self.verbose {
            tracing::info!("deploy: {}", args.join(" "));
        }

        tracing::info!(
            "flashing {} to {} via avrdude",
            firmware_path.display(),
            port
        );

        let result = run_command(
            &args_ref,
            None,
            None,
            Some(std::time::Duration::from_secs(60)),
        )?;

        if result.success() {
            Ok(DeploymentResult {
                success: true,
                message: format!("firmware flashed to {}", port),
                port: Some(port.to_string()),
            })
        } else {
            Err(fbuild_core::FbuildError::DeployFailed(format!(
                "avrdude failed:\n{}",
                result.stderr
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_avr_deployer_creation() {
        let deployer = AvrDeployer::new("atmega328p", "arduino", "115200", None, false);
        assert_eq!(deployer.mcu, "atmega328p");
        assert_eq!(deployer.programmer, "arduino");
        assert_eq!(deployer.baud_rate, "115200");
    }

    #[test]
    fn test_avr_deployer_from_board_config() {
        let board =
            fbuild_config::BoardConfig::from_board_id("uno", &std::collections::HashMap::new())
                .unwrap();
        let deployer = AvrDeployer::from_board_config(&board, false);
        assert_eq!(deployer.mcu, "atmega328p");
        assert_eq!(deployer.programmer, "arduino");
        assert_eq!(deployer.baud_rate, "115200");
    }

    #[test]
    fn test_deploy_requires_port() {
        let deployer = AvrDeployer::new("atmega328p", "arduino", "115200", None, false);
        let tmp = tempfile::TempDir::new().unwrap();
        let result = deployer.deploy(tmp.path(), "uno", Path::new("firmware.hex"), None);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("serial port required"));
    }
}
