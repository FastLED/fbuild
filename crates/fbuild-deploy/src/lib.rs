//! Firmware deployment via platform-specific upload tools.
//!
//! - AVR: avrdude
//! - ESP32: esptool
//! - RP2040: picotool
//! - STM32: st-flash / dfu-util
//! - Teensy: teensy_loader_cli

pub mod avr;
pub mod esp32;
/// Native espflash-backed verify/write path (issue #66).
///
/// Compiled in only when the `espflash-native` cargo feature is enabled.
/// Default builds keep the esptool-subprocess path and pay zero cost in
/// the dep graph (espflash pulls ~30 transitive crates: strum, deku,
/// miette, ...).
#[cfg(feature = "espflash-native")]
pub mod esp32_native;
pub mod reset;
pub mod teensy;

use fbuild_core::Result;
use std::path::Path;

use crate::esp32::FlashRegion;

/// What the deployer actually did on the device.
///
/// Surfaced through `DeploymentResult::outcome` so the daemon's
/// `/api/deploy` response message can distinguish between:
///
/// * a full baseline write (all regions / non-ESP platforms),
/// * a verify-skip (device already held the requested image), and
/// * a selective rewrite (only some ESP32 flash regions were written
///   because bootloader/partitions already matched).
///
/// See GitHub issue #76.
#[derive(Debug, Clone)]
pub enum DeployOutcome {
    /// All regions / the full image were written to the device.
    FullFlash,
    /// `esptool verify-flash` matched every region — no write was
    /// performed. The device has been hard-reset by esptool.
    VerifySkip,
    /// Only the listed ESP32 regions were rewritten. Order follows the
    /// caller's intent (usually bootloader → partitions → firmware).
    SelectiveFlash { regions: Vec<FlashRegion> },
}

impl DeployOutcome {
    /// Render a human-readable parenthetical suffix describing the
    /// outcome. Stable — consumers may parse it.
    ///
    /// * `FullFlash`        → `"full flash"`
    /// * `VerifySkip`       → `"verify skipped, device already matched"`
    /// * `SelectiveFlash`   → `"selective flash: firmware"`, etc.
    pub fn describe(&self) -> String {
        match self {
            DeployOutcome::FullFlash => "full flash".to_string(),
            DeployOutcome::VerifySkip => "verify skipped, device already matched".to_string(),
            DeployOutcome::SelectiveFlash { regions } => {
                let names: Vec<&'static str> = regions
                    .iter()
                    .map(|r| match r {
                        FlashRegion::Bootloader => "bootloader",
                        FlashRegion::Partitions => "partitions",
                        FlashRegion::Firmware => "firmware",
                    })
                    .collect();
                format!("selective flash: {}", names.join(", "))
            }
        }
    }
}

#[derive(Debug)]
pub struct DeploymentResult {
    pub success: bool,
    pub message: String,
    pub port: Option<String>,
    /// Captured stdout from the deploy tool (esptool, avrdude, etc.).
    pub stdout: String,
    /// Captured stderr from the deploy tool.
    pub stderr: String,
    /// What actually happened on the device (full / verify-skip /
    /// selective). Surfaced in the daemon's HTTP response message so
    /// consumers can tell an MD5-skip from a real write.
    pub outcome: DeployOutcome,
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

#[cfg(test)]
mod outcome_tests {
    use super::*;

    #[test]
    fn full_flash_describe() {
        assert_eq!(DeployOutcome::FullFlash.describe(), "full flash");
    }

    #[test]
    fn verify_skip_describe() {
        assert_eq!(
            DeployOutcome::VerifySkip.describe(),
            "verify skipped, device already matched"
        );
    }

    #[test]
    fn selective_flash_describe_firmware_only() {
        let outcome = DeployOutcome::SelectiveFlash {
            regions: vec![FlashRegion::Firmware],
        };
        assert_eq!(outcome.describe(), "selective flash: firmware");
    }

    #[test]
    fn selective_flash_describe_multiple_regions_ordered_and_lowercase() {
        let outcome = DeployOutcome::SelectiveFlash {
            regions: vec![FlashRegion::Bootloader, FlashRegion::Firmware],
        };
        // Lowercase names joined by ", " — see issue #76 contract.
        assert_eq!(outcome.describe(), "selective flash: bootloader, firmware");
    }

    #[test]
    fn selective_flash_describe_all_three_regions() {
        let outcome = DeployOutcome::SelectiveFlash {
            regions: vec![
                FlashRegion::Bootloader,
                FlashRegion::Partitions,
                FlashRegion::Firmware,
            ],
        };
        assert_eq!(
            outcome.describe(),
            "selective flash: bootloader, partitions, firmware"
        );
    }
}
