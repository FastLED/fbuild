//! Core types, errors, and utilities for fbuild.
//!
//! This crate provides the foundational types shared across all fbuild crates:
//! - Error types (FbuildError)
//! - Build profiles and enums
//! - Subprocess runner with platform-specific flags
//! - Size info parsing (avr-size / arm-none-eabi-size output)

pub mod subprocess;

use serde::{Deserialize, Serialize};

/// Top-level error type for fbuild operations.
#[derive(Debug, thiserror::Error)]
pub enum FbuildError {
    #[error("build failed: {0}")]
    BuildFailed(String),

    #[error("deploy failed: {0}")]
    DeployFailed(String),

    #[error("serial error: {0}")]
    SerialError(String),

    #[error("config error: {0}")]
    ConfigError(String),

    #[error("package error: {0}")]
    PackageError(String),

    #[error("daemon error: {0}")]
    DaemonError(String),

    #[error("timeout: {0}")]
    Timeout(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, FbuildError>;

/// Build profile selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BuildProfile {
    Release,
    Quick,
}

/// Platform identifier for orchestrator dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    AtmelAvr,
    Espressif32,
    Espressif8266,
    RaspberryPi,
    Ststm32,
    Teensy,
    Wasm,
}

impl Platform {
    /// Parse a platform string from platformio.ini (e.g. "atmelavr", "espressif32").
    pub fn from_platform_str(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "atmelavr" => Some(Self::AtmelAvr),
            "espressif32" => Some(Self::Espressif32),
            "espressif8266" => Some(Self::Espressif8266),
            "raspberrypi" | "raspberrypipico" => Some(Self::RaspberryPi),
            "ststm32" => Some(Self::Ststm32),
            "teensy" => Some(Self::Teensy),
            "wasm" | "emscripten" => Some(Self::Wasm),
            _ => None,
        }
    }
}

/// Operation types the daemon can process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationType {
    Build,
    Deploy,
    Monitor,
    BuildAndDeploy,
    InstallDependencies,
}

/// Daemon state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonState {
    Idle,
    Building,
    Deploying,
    Monitoring,
    Completed,
    Failed,
    Cancelled,
    Unknown,
}

/// Size information after a successful build.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SizeInfo {
    pub text: u64,
    pub data: u64,
    pub bss: u64,
    pub total_flash: u64,
    pub total_ram: u64,
    pub max_flash: Option<u64>,
    pub max_ram: Option<u64>,
}

impl SizeInfo {
    pub fn flash_percent(&self) -> Option<f64> {
        self.max_flash
            .map(|max| (self.total_flash as f64 / max as f64) * 100.0)
    }

    pub fn ram_percent(&self) -> Option<f64> {
        self.max_ram
            .map(|max| (self.total_ram as f64 / max as f64) * 100.0)
    }

    /// Parse size output from avr-size or arm-none-eabi-size.
    ///
    /// Supports two formats:
    /// 1. Berkeley format: `text  data  bss  dec  hex  filename`
    /// 2. AVR section format: `.section  size  address`
    pub fn parse(output: &str, max_flash: Option<u64>, max_ram: Option<u64>) -> Option<Self> {
        // Try Berkeley format first: look for a line with numeric columns
        // Format: "   text    data     bss     dec     hex filename"
        //         "   1234     56      78    1368     558 firmware.elf"
        for line in output.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 4 {
                if let (Ok(text), Ok(data), Ok(bss)) = (
                    parts[0].parse::<u64>(),
                    parts[1].parse::<u64>(),
                    parts[2].parse::<u64>(),
                ) {
                    return Some(Self {
                        text,
                        data,
                        bss,
                        total_flash: text + data,
                        total_ram: data + bss,
                        max_flash,
                        max_ram,
                    });
                }
            }
        }

        // Try AVR section format: ".text  1234  0x0"
        let mut text = 0u64;
        let mut data = 0u64;
        let mut bss = 0u64;
        let mut found = false;

        for line in output.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                if let Ok(size) = parts[1].parse::<u64>() {
                    match parts[0] {
                        ".text" => {
                            text = size;
                            found = true;
                        }
                        ".data" => {
                            data = size;
                            found = true;
                        }
                        ".bss" => {
                            bss = size;
                            found = true;
                        }
                        _ => {}
                    }
                }
            }
        }

        if found {
            Some(Self {
                text,
                data,
                bss,
                total_flash: text + data,
                total_ram: data + bss,
                max_flash,
                max_ram,
            })
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_info_percentages() {
        let info = SizeInfo {
            text: 1000,
            data: 200,
            bss: 100,
            total_flash: 1200,
            total_ram: 300,
            max_flash: Some(32768),
            max_ram: Some(2048),
        };
        assert!(info.flash_percent().unwrap() < 4.0);
        assert!(info.ram_percent().unwrap() < 15.0);
    }

    #[test]
    fn size_info_no_max() {
        let info = SizeInfo {
            text: 1000,
            data: 200,
            bss: 100,
            total_flash: 1200,
            total_ram: 300,
            max_flash: None,
            max_ram: None,
        };
        assert!(info.flash_percent().is_none());
        assert!(info.ram_percent().is_none());
    }

    #[test]
    fn size_info_parse_berkeley() {
        let output = "   text\t   data\t    bss\t    dec\t    hex\tfilename\n   \
                       924\t     14\t      9\t    947\t    3b3\tfirmware.elf\n";
        let info = SizeInfo::parse(output, Some(32256), Some(2048)).unwrap();
        assert_eq!(info.text, 924);
        assert_eq!(info.data, 14);
        assert_eq!(info.bss, 9);
        assert_eq!(info.total_flash, 938);
        assert_eq!(info.total_ram, 23);
    }

    #[test]
    fn size_info_parse_avr_sections() {
        let output = ".text   924   0x0\n.data   14   0x800100\n.bss   9   0x80010e\n";
        let info = SizeInfo::parse(output, Some(32256), Some(2048)).unwrap();
        assert_eq!(info.text, 924);
        assert_eq!(info.data, 14);
        assert_eq!(info.bss, 9);
    }

    #[test]
    fn size_info_parse_garbage_returns_none() {
        let output = "some random garbage\nnot a size output\n";
        assert!(SizeInfo::parse(output, None, None).is_none());
    }

    #[test]
    fn platform_from_str() {
        assert_eq!(
            Platform::from_platform_str("atmelavr"),
            Some(Platform::AtmelAvr)
        );
        assert_eq!(
            Platform::from_platform_str("espressif32"),
            Some(Platform::Espressif32)
        );
        assert_eq!(Platform::from_platform_str("unknown"), None);
    }
}
