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

pub use board::BoardConfig;
pub use ini_parser::PlatformIOConfig;
pub use mcu::McuSpec;
