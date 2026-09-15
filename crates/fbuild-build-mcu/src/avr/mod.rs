//! AVR platform build support (Arduino Uno, Mega, Nano, etc.)

pub mod avr_compiler;
pub mod avr_linker;
pub mod mcu_config;
pub mod orchestrator;

pub use avr_compiler::AvrCompiler;
pub use avr_linker::AvrLinker;
pub use orchestrator::AvrOrchestrator;

/// AVR platform support (AtmelAvr + AtmelMegaAvr).
pub struct AvrPlatformSupport;

#[async_trait::async_trait]
impl crate::PlatformSupport for AvrPlatformSupport {
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
            orchestrator::avr_packages(inputs.project_dir, Some(inputs.env_config), inputs.board)?;
        Ok(vec![
            provision_package(PackageKind::Toolchain, &toolchain, mode).await,
            provision_package(PackageKind::Framework, &framework, mode).await,
        ])
    }

    fn default_board_id(&self) -> &str {
        "uno"
    }
}
