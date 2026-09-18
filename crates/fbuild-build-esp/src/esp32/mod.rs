//! ESP32 platform build support (all variants: ESP32, C2, C3, C5, C6, P4, S3)

pub mod esp32_compiler;
pub mod esp32_linker;
pub mod mcu_config;
pub mod orchestrator;
pub(crate) mod size_report;

pub use esp32_compiler::Esp32Compiler;
pub use esp32_linker::Esp32Linker;
pub use mcu_config::Esp32McuConfig;
pub use orchestrator::Esp32Orchestrator;

/// ESP32 platform support.
pub struct Esp32PlatformSupport;

#[async_trait::async_trait]
impl crate::PlatformSupport for Esp32PlatformSupport {
    fn create_orchestrator(&self) -> Box<dyn crate::BuildOrchestrator> {
        orchestrator::create()
    }

    async fn provision(
        &self,
        inputs: &crate::provision::ProvisionInputs<'_>,
        mode: crate::provision::ProvisionMode,
    ) -> fbuild_core::Result<Vec<crate::provision::ProvisionedPackage>> {
        orchestrator::provision_esp32(inputs, mode).await
    }

    /// The Arduino core bundles libraries (FS, WiFi, ESPmDNS, ...) that
    /// `lib_deps` may name; the build filters them out before downloading,
    /// so provisioning must too (FastLED/fbuild#1442).
    fn downloadable_lib_deps(
        &self,
        inputs: &crate::provision::ProvisionInputs<'_>,
        lib_deps: Vec<String>,
    ) -> Vec<String> {
        orchestrator::downloadable_lib_deps(inputs, lib_deps)
    }

    fn default_board_id(&self) -> &str {
        "esp32dev"
    }
}
