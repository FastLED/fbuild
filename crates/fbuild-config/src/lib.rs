//! PlatformIO INI parser, board configuration, and MCU specs.
//!
//! Handles:
//! - platformio.ini parsing with environment inheritance (`extends = env:parent`)
//! - Variable substitution (`${env:parent.key}`)
//! - Board configuration from JSON
//! - MCU memory specs (flash/RAM limits)

pub mod board;
pub mod ini_parser;
pub mod mcu;
pub mod pio_env;
pub mod sdkconfig;

pub use board::{BoardConfig, DebugToolMeta, Esp32QemuPsramConfig};
pub use ini_parser::PlatformIOConfig;
pub use mcu::McuSpec;
pub use pio_env::{
    scan_unsupported, scan_warn_only, PioEnvOverrides, SUPPORTED_PIO_ENV_VARS,
    WARN_ONLY_PIO_ENV_VARS,
};
