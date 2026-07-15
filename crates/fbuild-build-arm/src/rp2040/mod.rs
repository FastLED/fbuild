//! RP2040/RP2350 platform build support (Raspberry Pi Pico, etc.)

pub mod mcu_config;
pub mod orchestrator;
mod uf2;

pub use orchestrator::Rp2040Orchestrator;

/// RP2040 platform support.
pub struct Rp2040PlatformSupport;

#[async_trait::async_trait]
impl crate::PlatformSupport for Rp2040PlatformSupport {
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
        "rpipico"
    }
}
