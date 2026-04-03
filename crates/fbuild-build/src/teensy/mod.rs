//! Teensy platform build support (Teensy 4.0, 4.1)

pub mod mcu_config;
pub mod orchestrator;
pub mod teensy_compiler;
pub mod teensy_linker;

pub use orchestrator::TeensyOrchestrator;
pub use teensy_compiler::TeensyCompiler;
pub use teensy_linker::TeensyLinker;

/// Teensy platform support.
pub struct TeensyPlatformSupport;

impl crate::PlatformSupport for TeensyPlatformSupport {
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
        "teensy41"
    }
}
