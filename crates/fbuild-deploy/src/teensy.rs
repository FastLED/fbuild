//! Teensy deployer using teensy_loader_cli.
//!
//! Flashes firmware.hex to Teensy boards via USB.

use std::path::{Path, PathBuf};

use fbuild_core::subprocess::run_command;
use fbuild_core::Result;

use crate::{Deployer, DeploymentResult};

/// Teensy deployer using teensy_loader_cli.
pub struct TeensyDeployer {
    /// Path to teensy_loader_cli binary (if not in PATH).
    loader_path: PathBuf,
    /// MCU name for --mcu flag (e.g. "TEENSY41").
    mcu_name: String,
    verbose: bool,
}

impl TeensyDeployer {
    pub fn new(mcu_name: &str, loader_path: Option<PathBuf>, verbose: bool) -> Self {
        Self {
            loader_path: loader_path.unwrap_or_else(|| PathBuf::from("teensy_loader_cli")),
            mcu_name: mcu_name.to_string(),
            verbose,
        }
    }

    /// Create a Teensy deployer from board config defaults.
    ///
    /// MCU name is the uppercase board ID (e.g. "TEENSY41").
    pub fn from_board_config(board: &fbuild_config::BoardConfig, verbose: bool) -> Self {
        Self::new(&board.board.to_uppercase(), None, verbose)
    }
}

impl Deployer for TeensyDeployer {
    fn deploy(
        &self,
        _project_dir: &Path,
        _env_name: &str,
        firmware_path: &Path,
        port: Option<&str>,
    ) -> Result<DeploymentResult> {
        // Teensy doesn't strictly require a port (uses USB HID),
        // but we accept it for interface consistency.
        let _ = port;

        let args = [
            self.loader_path.to_string_lossy().to_string(),
            format!("--mcu={}", self.mcu_name),
            "-w".to_string(),
            "-v".to_string(),
            firmware_path.to_string_lossy().to_string(),
        ];

        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

        if self.verbose {
            tracing::info!("deploy: {}", args.join(" "));
        }

        tracing::info!(
            "flashing {} via teensy_loader_cli ({})",
            firmware_path.display(),
            self.mcu_name
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
                message: format!("firmware flashed to {}", self.mcu_name),
                port: port.map(|p| p.to_string()),
            })
        } else {
            Err(fbuild_core::FbuildError::DeployFailed(format!(
                "teensy_loader_cli failed:\n{}",
                result.stderr
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_teensy_deployer_creation() {
        let deployer = TeensyDeployer::new("TEENSY41", None, false);
        assert_eq!(deployer.mcu_name, "TEENSY41");
    }

    #[test]
    fn test_teensy_deployer_from_board_config() {
        let board = fbuild_config::BoardConfig::from_board_id(
            "teensy41",
            &std::collections::HashMap::new(),
        )
        .unwrap();
        let deployer = TeensyDeployer::from_board_config(&board, false);
        assert_eq!(deployer.mcu_name, "TEENSY41");
    }

    #[test]
    fn test_teensy_deployer_teensy40() {
        let board = fbuild_config::BoardConfig::from_board_id(
            "teensy40",
            &std::collections::HashMap::new(),
        )
        .unwrap();
        let deployer = TeensyDeployer::from_board_config(&board, false);
        assert_eq!(deployer.mcu_name, "TEENSY40");
    }
}
