//! Renesas RA platform build support (Arduino UNO R4, etc.)

mod framework_libs;
pub mod mcu_config;
pub mod orchestrator;
pub mod renesas_compiler;
pub mod renesas_linker;

pub use orchestrator::RenesasOrchestrator;
pub use renesas_compiler::RenesasCompiler;
pub use renesas_linker::RenesasLinker;

/// Renesas RA platform support.
pub struct RenesasPlatformSupport;

#[async_trait::async_trait]
impl crate::PlatformSupport for RenesasPlatformSupport {
    fn create_orchestrator(&self) -> Box<dyn crate::BuildOrchestrator> {
        orchestrator::create()
    }

    async fn provision(
        &self,
        inputs: &crate::provision::ProvisionInputs<'_>,
        mode: crate::provision::ProvisionMode,
    ) -> fbuild_core::Result<Vec<crate::provision::ProvisionedPackage>> {
        use crate::provision::{PackageKind, provision_package};
        // The core package follows the board's MCU family — `renesas_uno` for
        // RA4M1 boards, `renesas_portenta` for RA6M5 (Portenta C33). See
        // FastLED/fbuild#2054.
        let mcu_family = mcu_config::renesas_mcu_family(&inputs.board.mcu)?;
        let core_kind = fbuild_packages::library::RenesasCoreKind::for_family(mcu_family);
        let (toolchain, cores) =
            orchestrator::renesas_packages(inputs.project_dir, Some(inputs.env_config), core_kind)
                .await?;
        Ok(vec![
            provision_package(PackageKind::Toolchain, &toolchain, mode).await,
            provision_package(PackageKind::Framework, &cores, mode).await,
        ])
    }

    fn default_board_id(&self) -> &str {
        "uno_r4_wifi"
    }
}
