//! Nordic nRF51 platform build support (Cortex-M0, sandeepmistry arduino-nRF5).

pub mod mcu_config;
pub mod orchestrator;

pub use orchestrator::Nrf51Orchestrator;

/// nRF51 platform support.
pub struct Nrf51PlatformSupport;

#[async_trait::async_trait]
impl crate::PlatformSupport for Nrf51PlatformSupport {
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
            orchestrator::nrf51_packages(inputs.project_dir, Some(inputs.env_config)).await?;
        Ok(vec![
            provision_package(PackageKind::Toolchain, &toolchain, mode).await,
            provision_package(PackageKind::Framework, &cores, mode).await,
        ])
    }

    fn default_board_id(&self) -> &str {
        "bbcmicrobit"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PlatformSupport;

    #[test]
    fn default_board_is_bbcmicrobit() {
        assert_eq!(Nrf51PlatformSupport.default_board_id(), "bbcmicrobit");
    }

    #[test]
    fn creates_nrf51_orchestrator() {
        let orch = Nrf51PlatformSupport.create_orchestrator();
        assert_eq!(orch.platform(), fbuild_core::Platform::NordicNrf51);
    }
}
