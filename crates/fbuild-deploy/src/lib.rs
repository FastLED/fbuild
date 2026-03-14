//! Firmware deployment via platform-specific upload tools.
//!
//! - AVR: avrdude
//! - ESP32: esptool
//! - RP2040: picotool
//! - STM32: st-flash / dfu-util
//! - Teensy: teensy_loader_cli

pub mod avr;
pub mod teensy;

use fbuild_core::Result;
use std::path::Path;

#[derive(Debug)]
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
