//! ESP8266 platform build support (NodeMCU, Wemos D1, etc.)

mod board_props;
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
        use crate::provision::{
            PackageKind, ProvisionStatus, ProvisionedPackage, provision_package,
        };
        let selected = if mode.fetches() {
            Some(orchestrator::esp8266_packages(inputs.project_dir, Some(inputs.env_config)).await?)
        } else {
            orchestrator::esp8266_packages_offline(inputs.project_dir, Some(inputs.env_config))
                .await?
        };
        let Some((toolchain, framework)) = selected else {
            // Metadata or the selected platform manifest is not cached.
            // These modes cannot fetch it, so report uncertainty as missing
            // instead of silently reporting the adapter's default packages.
            return Ok(vec![
                ProvisionedPackage::new(
                    PackageKind::Toolchain,
                    "toolchain-xtensa",
                    ProvisionStatus::WouldFetch,
                ),
                ProvisionedPackage::new(
                    PackageKind::Framework,
                    "framework-arduinoespressif8266",
                    ProvisionStatus::WouldFetch,
                ),
            ]);
        };
        Ok(vec![
            provision_package(PackageKind::Toolchain, &toolchain, mode).await,
            provision_package(PackageKind::Framework, &framework, mode).await,
        ])
    }

    fn default_board_id(&self) -> &str {
        "nodemcuv2"
    }
}
