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

#[async_trait::async_trait]
impl crate::PlatformSupport for TeensyPlatformSupport {
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
            orchestrator::teensy_packages(inputs.project_dir, Some(inputs.env_config));
        Ok(vec![
            provision_package(PackageKind::Toolchain, &toolchain, mode).await,
            provision_package(PackageKind::Framework, &cores, mode).await,
        ])
    }

    fn default_board_id(&self) -> &str {
        "teensy41"
    }
}
