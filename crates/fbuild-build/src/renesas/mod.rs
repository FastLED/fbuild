//! Renesas RA platform build support (Arduino UNO R4, etc.)

pub mod mcu_config;
pub mod orchestrator;
pub mod renesas_compiler;
pub mod renesas_linker;

pub use orchestrator::RenesasOrchestrator;
pub use renesas_compiler::RenesasCompiler;
pub use renesas_linker::RenesasLinker;

/// Renesas RA platform support.
pub struct RenesasPlatformSupport;

impl crate::PlatformSupport for RenesasPlatformSupport {
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
        "uno_r4_wifi"
    }
}
