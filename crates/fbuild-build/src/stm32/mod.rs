//! STM32 platform build support (STM32F1, STM32F4, STM32H7, etc.)

pub mod mcu_config;
pub mod orchestrator;

pub use orchestrator::Stm32Orchestrator;

/// STM32 platform support.
pub struct Stm32PlatformSupport;

#[async_trait::async_trait]
impl crate::PlatformSupport for Stm32PlatformSupport {
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
        "bluepill_f103c8"
    }
}
