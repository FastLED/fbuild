//! SAM platform build support (Atmel SAM3X8E / Arduino Due)

pub mod mcu_config;
pub mod orchestrator;
pub mod sam_compiler;
pub mod sam_linker;

pub use orchestrator::SamOrchestrator;
pub use sam_compiler::SamCompiler;
pub use sam_linker::SamLinker;

/// SAM platform support.
pub struct SamPlatformSupport;

#[async_trait::async_trait]
impl crate::PlatformSupport for SamPlatformSupport {
    fn create_orchestrator(&self) -> Box<dyn crate::BuildOrchestrator> {
        orchestrator::create()
    }

    async fn install_deps(&self, project_dir: &std::path::Path) -> fbuild_core::Result<()> {
        use fbuild_packages::Package;
        let tc = fbuild_packages::toolchain::ArmToolchain::new(project_dir);
        Package::ensure_installed(&tc).await?;
        tracing::info!("ARM toolchain installed");
        Ok(())
    }

    fn default_board_id(&self) -> &str {
        "due"
    }
}
