//! Toolchain package management — AVR-GCC, ARM GCC, ESP32, RISC-V, clang tools, and other platform toolchains.

pub mod arm;
pub mod arm_gcc8;
pub mod avr;
pub mod clang;
pub mod esp32;
pub mod esp32_metadata;
pub mod esp8266;
pub mod riscv;

pub use arm::ArmToolchain;
pub use arm_gcc8::ArmGcc8Toolchain;
pub use avr::AvrToolchain;
pub use clang::{ClangComponent, ClangComponentKind};
pub use esp32::Esp32Toolchain;
pub use esp8266::Esp8266Toolchain;
pub use riscv::RiscvToolchain;
