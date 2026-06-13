//! NXP LPC8xx (Cortex-M0+) bare-metal build support.
//!
//! - Stage 1 (shipped): board/toolchain wiring, board JSON, linker scripts,
//!   startup `.S`, dispatch table entry.
//! - Stage 2 (this module): real build orchestrator that compiles user
//!   sources + the embedded `main.cpp` shim + startup `.S` and links
//!   against the per-MCU linker script. See [`orchestrator`].
//! - Stage 3 (#479 / [`zackees/ArduinoCore-LPC8xx`](https://github.com/zackees/ArduinoCore-LPC8xx)):
//!   replace the embedded shim with a real Arduino framework.
//!
//! Tracked under #487.

pub mod mcu_config;
pub mod orchestrator;

use std::path::Path;

use fbuild_core::Result;

/// Linker script for LPC804 (32 KB Flash, 4 KB RAM).
///
/// Origins / sizes are taken from the LPC804 datasheet, table 5
/// ("Memory mapping"). Standard Cortex-M0+ section layout.
pub const LPC804_LD: &str = include_str!("assets/lpc804.ld");

/// Linker script for LPC845 (64 KB Flash, 16 KB RAM).
///
/// Origins / sizes are taken from the LPC84x datasheet, section 7.6
/// ("Memory map"). Standard Cortex-M0+ section layout.
pub const LPC845_LD: &str = include_str!("assets/lpc845.ld");

/// Minimal Reset_Handler + vector table for LPC804.
pub const LPC804_STARTUP: &str = include_str!("assets/startup_lpc804.S");

/// Minimal Reset_Handler + vector table for LPC845.
pub const LPC845_STARTUP: &str = include_str!("assets/startup_lpc845.S");

/// Hand-rolled Arduino `main()` shim. Wraps the user-provided `setup()`
/// and `loop()` into the canonical `int main() { setup(); for(;;) loop(); }`
/// pattern. Materialised to the build dir by the orchestrator and
/// compiled alongside the user sketch. Replaced by the framework-owned
/// `main.cpp` once #479 / ArduinoCore-LPC8xx is vendored (Stage 4 of #487).
pub const MAIN_CPP_SHIM: &str = include_str!("assets/main.cpp");

/// Minimal Arduino compatibility layer used until #479's external
/// ArduinoCore-LPC8xx package is production-ready.
pub const ARDUINO_STUB_ASSETS: &[(&str, &str)] = &[
    (
        "arduino_stub/Arduino.h",
        include_str!("assets/arduino_stub/Arduino.h"),
    ),
    (
        "arduino_stub/HardwareSerial.h",
        include_str!("assets/arduino_stub/HardwareSerial.h"),
    ),
    (
        "arduino_stub/HardwareSerial.cpp",
        include_str!("assets/arduino_stub/HardwareSerial.cpp"),
    ),
    (
        "arduino_stub/SPI.h",
        include_str!("assets/arduino_stub/SPI.h"),
    ),
    (
        "arduino_stub/SPI.cpp",
        include_str!("assets/arduino_stub/SPI.cpp"),
    ),
    (
        "arduino_stub/wiring_digital.c",
        include_str!("assets/arduino_stub/wiring_digital.c"),
    ),
    (
        "arduino_stub/wiring_time.c",
        include_str!("assets/arduino_stub/wiring_time.c"),
    ),
    (
        "arduino_stub/new_delete.cpp",
        include_str!("assets/arduino_stub/new_delete.cpp"),
    ),
];

/// NXP device headers that bridge the generic CMSIS Core package to the
/// LPC804/LPC845-specific symbols FastLED includes (`LPC804.h`, `LPC845.h`,
/// and `fsl_device_registers.h`).
pub const DEVICE_HEADER_ASSETS: &[(&str, &str)] = &[
    (
        "device_headers/LPC804.h",
        include_str!("assets/device_headers/LPC804.h"),
    ),
    (
        "device_headers/LPC845.h",
        include_str!("assets/device_headers/LPC845.h"),
    ),
    (
        "device_headers/fsl_device_registers.h",
        include_str!("assets/device_headers/fsl_device_registers.h"),
    ),
    (
        "device_headers/system_LPC804.h",
        include_str!("assets/device_headers/system_LPC804.h"),
    ),
    (
        "device_headers/system_LPC845.h",
        include_str!("assets/device_headers/system_LPC845.h"),
    ),
];

/// NXP LPC8xx platform support.
pub struct NxpLpcPlatformSupport;

impl crate::PlatformSupport for NxpLpcPlatformSupport {
    fn create_orchestrator(&self) -> Box<dyn crate::BuildOrchestrator> {
        orchestrator::create()
    }

    fn install_deps(&self, project_dir: &Path) -> Result<()> {
        // ARM GCC is the right toolchain for Cortex-M0+ bare metal.
        // Pre-install it so the orchestrator can `ensure_installed` cheaply.
        use fbuild_packages::Package;
        let tc = fbuild_packages::toolchain::ArmToolchain::new(project_dir);
        Package::ensure_installed(&tc)?;
        let cmsis = fbuild_packages::library::CmsisFramework::new(project_dir);
        Package::ensure_installed(&cmsis)?;
        tracing::info!("ARM GCC toolchain installed for NXP LPC8xx");
        Ok(())
    }

    fn default_board_id(&self) -> &str {
        "lpc845"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linker_scripts_have_expected_memory_regions() {
        assert!(LPC804_LD.contains("FLASH"));
        assert!(LPC804_LD.contains("0x00000000"));
        assert!(LPC804_LD.contains("LENGTH = 32K"));
        assert!(LPC804_LD.contains("0x10000000"));
        assert!(LPC804_LD.contains("LENGTH = 4K"));

        assert!(LPC845_LD.contains("FLASH"));
        assert!(LPC845_LD.contains("LENGTH = 64K"));
        assert!(LPC845_LD.contains("LENGTH = 16K"));
    }

    #[test]
    fn startup_files_define_reset_handler() {
        assert!(LPC804_STARTUP.contains("Reset_Handler"));
        assert!(LPC845_STARTUP.contains("Reset_Handler"));
    }

    #[test]
    fn main_cpp_shim_is_non_empty() {
        // The orchestrator depends on this asset existing; assert it
        // wasn't accidentally emptied.
        assert!(!MAIN_CPP_SHIM.trim().is_empty());
    }
}
