//! Apollo3 platform build support (Ambiq Micro Apollo3 / SparkFun Artemis).

pub mod mcu_config;
pub mod orchestrator;

pub use orchestrator::Apollo3Orchestrator;

/// Apollo3 platform support.
pub struct Apollo3PlatformSupport;

#[async_trait::async_trait]
impl crate::PlatformSupport for Apollo3PlatformSupport {
    fn create_orchestrator(&self) -> Box<dyn crate::BuildOrchestrator> {
        orchestrator::create()
    }

    async fn install_deps(&self, project_dir: &std::path::Path) -> fbuild_core::Result<()> {
        use fbuild_packages::Package;
        let tc = fbuild_packages::toolchain::ArmGcc8Toolchain::new(project_dir);
        Package::ensure_installed(&tc).await?;
        tracing::info!("ARM GCC 8 toolchain installed");
        Ok(())
    }

    fn default_board_id(&self) -> &str {
        "SparkFun_RedBoard_Artemis_ATP"
    }
}
