//! Toolchain package management — AVR-GCC, ARM GCC, ESP32, clang tools, and other platform toolchains.

pub mod arm;
pub mod avr;
pub mod clang;
pub mod esp32;
pub mod esp32_metadata;
pub mod esp8266;

pub use arm::ArmToolchain;
pub use avr::AvrToolchain;
pub use clang::{ClangComponent, ClangComponentKind};
pub use esp32::Esp32Toolchain;
pub use esp8266::Esp8266Toolchain;
