//! ESP8266 platform build support (NodeMCU, Wemos D1, etc.)

pub mod esp8266_compiler;
pub mod esp8266_linker;
pub mod mcu_config;
pub mod orchestrator;

pub use esp8266_compiler::Esp8266Compiler;
pub use esp8266_linker::Esp8266Linker;
pub use orchestrator::Esp8266Orchestrator;

/// ESP8266 platform support.
pub struct Esp8266PlatformSupport;

#[async_trait::async_trait]
impl crate::PlatformSupport for Esp8266PlatformSupport {
    fn create_orchestrator(&self) -> Box<dyn crate::BuildOrchestrator> {
        orchestrator::create()
    }

    async fn provision(
        &self,
        inputs: &crate::provision::ProvisionInputs<'_>,
        mode: crate::provision::ProvisionMode,
    ) -> fbuild_core::Result<Vec<crate::provision::ProvisionedPackage>> {
        use crate::provision::{PackageKind, provision_package};
        let (toolchain, framework) =
            orchestrator::esp8266_packages(inputs.project_dir, Some(inputs.env_config));
        Ok(vec![
            provision_package(PackageKind::Toolchain, &toolchain, mode).await,
            provision_package(PackageKind::Framework, &framework, mode).await,
        ])
    }

    fn default_board_id(&self) -> &str {
        "nodemcuv2"
    }
}
