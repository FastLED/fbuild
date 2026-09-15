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

    async fn provision(
        &self,
        inputs: &crate::provision::ProvisionInputs<'_>,
        mode: crate::provision::ProvisionMode,
    ) -> fbuild_core::Result<Vec<crate::provision::ProvisionedPackage>> {
        use crate::provision::{PackageKind, provision_package};
        let (toolchain, core) =
            orchestrator::stm32_packages(inputs.project_dir, Some(inputs.env_config), inputs.board);
        let mut rows = vec![provision_package(PackageKind::Toolchain, &toolchain, mode).await];
        match core {
            orchestrator::Stm32Core::Stm32duino { cores, cmsis } => {
                rows.push(provision_package(PackageKind::Framework, &cores, mode).await);
                rows.push(provision_package(PackageKind::Framework, &cmsis, mode).await);
            }
            orchestrator::Stm32Core::ArduinoMbed(core) => {
                rows.push(provision_package(PackageKind::Framework, &core, mode).await);
            }
        }
        Ok(rows)
    }

    fn default_board_id(&self) -> &str {
        "bluepill_f103c8"
    }
}
