//! NRF52 platform build support (Nordic NRF52840, etc.)

pub mod mcu_config;
pub mod nrf52_compiler;
pub mod nrf52_linker;
pub mod orchestrator;

pub use nrf52_compiler::Nrf52Compiler;
pub use nrf52_linker::Nrf52Linker;
pub use orchestrator::Nrf52Orchestrator;

/// NRF52 platform support.
pub struct Nrf52PlatformSupport;

#[async_trait::async_trait]
impl crate::PlatformSupport for Nrf52PlatformSupport {
    fn create_orchestrator(&self) -> Box<dyn crate::BuildOrchestrator> {
        orchestrator::create()
    }

    async fn provision(
        &self,
        inputs: &crate::provision::ProvisionInputs<'_>,
        mode: crate::provision::ProvisionMode,
    ) -> fbuild_core::Result<Vec<crate::provision::ProvisionedPackage>> {
        use crate::provision::{PackageKind, provision_package};
        let (toolchain, cores, cmsis) =
            orchestrator::nrf52_packages(inputs.project_dir, Some(inputs.env_config));
        Ok(vec![
            provision_package(PackageKind::Toolchain, &toolchain, mode).await,
            provision_package(PackageKind::Framework, &cores, mode).await,
            provision_package(PackageKind::Framework, &cmsis, mode).await,
        ])
    }

    fn default_board_id(&self) -> &str {
        "nrf52840_dk"
    }
}
