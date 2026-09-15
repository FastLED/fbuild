//! CH32V RISC-V platform build support (WCH CH32V003, CH32V203, etc.)

pub mod ch32v_compiler;
pub mod ch32v_linker;
pub mod mcu_config;
pub mod orchestrator;

pub use ch32v_compiler::Ch32vCompiler;
pub use ch32v_linker::Ch32vLinker;
pub use orchestrator::Ch32vOrchestrator;

/// CH32V platform support.
pub struct Ch32vPlatformSupport;

#[async_trait::async_trait]
impl crate::PlatformSupport for Ch32vPlatformSupport {
    fn create_orchestrator(&self) -> Box<dyn crate::BuildOrchestrator> {
        orchestrator::create()
    }

    async fn provision(
        &self,
        inputs: &crate::provision::ProvisionInputs<'_>,
        mode: crate::provision::ProvisionMode,
    ) -> fbuild_core::Result<Vec<crate::provision::ProvisionedPackage>> {
        use crate::provision::{PackageKind, provision_package};
        // Reject what the build would reject before downloading for it.
        orchestrator::validate_ch32v_framework(
            inputs.env_config.get("framework").map(String::as_str),
        )?;
        let (toolchain, cores) =
            orchestrator::ch32v_packages(inputs.project_dir, Some(inputs.env_config));
        Ok(vec![
            provision_package(PackageKind::Toolchain, &toolchain, mode).await,
            provision_package(PackageKind::Framework, &cores, mode).await,
        ])
    }

    fn default_board_id(&self) -> &str {
        "genericCH32V003F4P6"
    }
}
