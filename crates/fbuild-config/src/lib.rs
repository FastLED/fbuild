//! PlatformIO INI parser, board configuration, and MCU specs.
//!
//! Handles:
//! - platformio.ini parsing with environment inheritance (`extends = env:parent`)
//! - Variable substitution (`${env:parent.key}`)
//! - Board configuration from JSON
//! - MCU memory specs (flash/RAM limits)

pub mod board;
pub mod ini_parser;
pub mod lib_source;
pub mod mcu;
pub mod pio_env;
pub mod platform_packages;
pub mod sdkconfig;

pub use board::{BoardConfig, BoardSummary, DebugToolMeta, Esp32QemuPsramConfig, search_boards};
pub use ini_parser::PlatformIOConfig;
pub use lib_source::{ClassifiedDep, LockStatus, SourceType, classify as classify_lib_dep};
pub use mcu::McuSpec;
pub use pio_env::{
    PioEnvOverrides, SUPPORTED_PIO_ENV_VARS, WARN_ONLY_PIO_ENV_VARS, scan_unsupported,
    scan_warn_only,
};
pub use platform_packages::{
    PackageOverride, parse_platform_packages_entry, parse_platform_packages_value,
};
