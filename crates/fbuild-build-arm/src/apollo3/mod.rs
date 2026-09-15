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

    async fn provision(
        &self,
        inputs: &crate::provision::ProvisionInputs<'_>,
        mode: crate::provision::ProvisionMode,
    ) -> fbuild_core::Result<Vec<crate::provision::ProvisionedPackage>> {
        use crate::provision::{PackageKind, provision_package};
        let (toolchain, cores) =
            orchestrator::apollo3_packages(inputs.project_dir, Some(inputs.env_config));
        Ok(vec![
            provision_package(PackageKind::Toolchain, &toolchain, mode).await,
            provision_package(PackageKind::Framework, &cores, mode).await,
        ])
    }

    fn default_board_id(&self) -> &str {
        "SparkFun_RedBoard_Artemis_ATP"
    }
}
