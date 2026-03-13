//! Core types, errors, and utilities for fbuild.
//!
//! This crate provides the foundational types shared across all fbuild crates:
//! - Error types (FbuildError)
//! - Build profiles and enums
//! - Common result type aliases

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
}
