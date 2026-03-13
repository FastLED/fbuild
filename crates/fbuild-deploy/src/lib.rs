//! Firmware deployment via platform-specific upload tools.
//!
//! - ESP32: esptool
//! - AVR: avrdude
//! - RP2040: picotool
//! - STM32: st-flash / dfu-util
//! - Teensy: teensy_loader_cli

use fbuild_core::Result;
use std::path::Path;

pub struct DeploymentResult {
    pub success: bool,
    pub message: String,
    pub port: Option<String>,
}

/// Trait for platform-specific deployers.
pub trait Deployer: Send + Sync {
    fn deploy(
        &self,
        project_dir: &Path,
        env_name: &str,
        firmware_path: &Path,
        port: Option<&str>,
    ) -> Result<DeploymentResult>;
}
