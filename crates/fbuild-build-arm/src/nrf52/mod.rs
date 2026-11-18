//! NRF52 platform build support (Nordic NRF52840, etc.)

mod core_select;
pub mod mcu_config;
pub mod nrf52_compiler;
pub mod nrf52_linker;
pub mod orchestrator;

pub use nrf52_compiler::Nrf52Compiler;
pub use nrf52_linker::Nrf52Linker;
pub use orchestrator::Nrf52Orchestrator;

/// The nRF52 core (and which one) a build of `board` compiles against, without downloading
/// it: the board's `build.framework_package` (FastLED/fbuild#2017) with the
/// env's `platform_packages` override for that package. `fbuild lib-select`
/// uses this so it inspects the same core's `libraries/` as the build
/// (FastLED/fbuild#2042). A registry pin (`platform = nordicnrf52@x.y.z`) is
/// not resolved here: that needs the registry, and lib-select stays offline.
pub fn installed_cores(
    project_dir: &std::path::Path,
    env_config: &std::collections::HashMap<String, String>,
    board: &fbuild_config::BoardConfig,
) -> fbuild_core::Result<(
    fbuild_packages::library::Nrf52Framework,
    fbuild_packages::library::Nrf52Cores,
)> {
    let framework = core_select::selected_framework(board)?;
    let (_, cores, _) = orchestrator::nrf52_packages_from_resolved(
        project_dir,
        Some(env_config),
        framework,
        &std::collections::HashMap::new(),
    );
    Ok((framework, cores))
}

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
            orchestrator::nrf52_packages(inputs.project_dir, Some(inputs.env_config), inputs.board)
                .await?;
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
