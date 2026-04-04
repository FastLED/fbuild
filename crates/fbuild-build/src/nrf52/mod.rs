//! NRF52 platform build support (Nordic NRF52840, etc.)

pub mod mcu_config;
pub mod nrf52_compiler;
pub mod nrf52_linker;
pub mod orchestrator;

pub use nrf52_compiler::Nrf52Compiler;
pub use nrf52_linker::Nrf52Linker;
pub use orchestrator::Nrf52Orchestrator;

/// NRF52 platform support.
pub struct Nrf52PlatformSupport;

impl crate::PlatformSupport for Nrf52PlatformSupport {
    fn create_orchestrator(&self) -> Box<dyn crate::BuildOrchestrator> {
        orchestrator::create()
    }

    fn install_deps(&self, project_dir: &std::path::Path) -> fbuild_core::Result<()> {
        use fbuild_packages::Package;
        let tc = fbuild_packages::toolchain::ArmToolchain::new(project_dir);
        Package::ensure_installed(&tc)?;
        tracing::info!("ARM toolchain installed");
        Ok(())
    }

    fn default_board_id(&self) -> &str {
        "nrf52840_dk"
    }
}
