//! AVR platform build support (Arduino Uno, Mega, Nano, etc.)

pub mod avr_compiler;
pub mod avr_linker;
pub mod mcu_config;
pub mod orchestrator;

pub use avr_compiler::AvrCompiler;
pub use avr_linker::AvrLinker;
pub use orchestrator::AvrOrchestrator;

/// AVR platform support (AtmelAvr + AtmelMegaAvr).
pub struct AvrPlatformSupport;

#[async_trait::async_trait]
impl crate::PlatformSupport for AvrPlatformSupport {
    fn create_orchestrator(&self) -> Box<dyn crate::BuildOrchestrator> {
        orchestrator::create()
    }

    async fn install_deps(&self, project_dir: &std::path::Path) -> fbuild_core::Result<()> {
        use fbuild_packages::Package;
        let tc = fbuild_packages::toolchain::AvrToolchain::new(project_dir);
        Package::ensure_installed(&tc).await?;
        tracing::info!("AVR toolchain installed");
        Ok(())
    }

    fn default_board_id(&self) -> &str {
        "uno"
    }
}
