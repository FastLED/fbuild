//! Generic ARM Cortex-M build support shared across STM32, RP2040, NRF52, SAM, etc.

pub mod arm_compiler;
pub mod arm_linker;
pub mod mcu_config;

pub use arm_compiler::ArmCompiler;
pub use arm_linker::ArmLinker;
pub use mcu_config::ArmMcuConfig;
