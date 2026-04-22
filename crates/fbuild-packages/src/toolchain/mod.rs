//! Toolchain package management — AVR-GCC, ARM GCC, ESP32, RISC-V, clang tools, and other platform toolchains.

pub mod arm;
pub mod arm_gcc8;
pub mod avr;
pub mod clang;
pub mod esp32;
pub mod esp32_metadata;
pub mod esp8266;
pub mod esp_qemu;
pub mod riscv;
pub mod rp2040_pqt;
pub mod teensy_arm;

pub use arm::ArmToolchain;
pub use arm_gcc8::ArmGcc8Toolchain;
pub use avr::AvrToolchain;
pub use clang::{ClangComponent, ClangComponentKind};
pub use esp32::Esp32Toolchain;
pub use esp8266::Esp8266Toolchain;
#[cfg(windows)]
pub use esp_qemu::build_windows_qemu_path_env;
pub use esp_qemu::{EspQemu, EspQemuArch, EspQemuRiscv32, EspQemuXtensa};
pub use riscv::RiscvToolchain;
pub use rp2040_pqt::Rp2040PqtToolchain;
pub use teensy_arm::TeensyArmToolchain;
