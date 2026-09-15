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

#[async_trait::async_trait]
impl crate::PlatformSupport for SilabsPlatformSupport {
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
            orchestrator::silabs_packages(inputs.project_dir, Some(inputs.env_config));
        Ok(vec![
            provision_package(PackageKind::Toolchain, &toolchain, mode).await,
            provision_package(PackageKind::Framework, &cores, mode).await,
        ])
    }

    fn default_board_id(&self) -> &str {
        "sparkfun_thingplusmatter"
    }
}
