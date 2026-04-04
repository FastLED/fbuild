//! Silicon Labs platform build support (EFR32MG24 / SparkFun Thing Plus Matter, etc.)

pub mod mcu_config;
pub mod orchestrator;
pub mod silabs_compiler;
pub mod silabs_linker;

pub use orchestrator::SilabsOrchestrator;
pub use silabs_compiler::SilabsCompiler;
pub use silabs_linker::SilabsLinker;

/// Silicon Labs platform support.
pub struct SilabsPlatformSupport;

impl crate::PlatformSupport for SilabsPlatformSupport {
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
        "sparkfun_thingplusmatter"
    }
}
