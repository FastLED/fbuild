//! ESP32 platform build support (all variants: ESP32, C2, C3, C5, C6, P4, S3)

pub mod esp32_compiler;
pub mod esp32_linker;
pub mod mcu_config;
pub mod orchestrator;

pub use esp32_compiler::Esp32Compiler;
pub use esp32_linker::Esp32Linker;
pub use mcu_config::Esp32McuConfig;
pub use orchestrator::Esp32Orchestrator;

/// ESP32 platform support.
pub struct Esp32PlatformSupport;

impl crate::PlatformSupport for Esp32PlatformSupport {
    fn create_orchestrator(&self) -> Box<dyn crate::BuildOrchestrator> {
        orchestrator::create()
    }

    fn install_deps(&self, project_dir: &std::path::Path) -> fbuild_core::Result<()> {
        use fbuild_packages::Package;
        let tc = fbuild_packages::toolchain::esp32::Esp32Toolchain::new(
            project_dir,
            false,
            "xtensa-esp-elf",
        );
        Package::ensure_installed(&tc)?;
        tracing::info!("ESP32 toolchain installed");
        Ok(())
    }

    fn default_board_id(&self) -> &str {
        "esp32dev"
    }
}
