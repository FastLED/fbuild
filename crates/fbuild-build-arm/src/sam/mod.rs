//! SAM platform build support (Atmel SAM3X8E / Arduino Due)

pub mod mcu_config;
pub mod orchestrator;
pub mod sam_compiler;
pub mod sam_linker;

pub use orchestrator::SamOrchestrator;
pub use sam_compiler::SamCompiler;
pub use sam_linker::SamLinker;

/// SAM platform support.
pub struct SamPlatformSupport;

#[async_trait::async_trait]
impl crate::PlatformSupport for SamPlatformSupport {
    fn create_orchestrator(&self) -> Box<dyn crate::BuildOrchestrator> {
        orchestrator::create()
    }

    async fn provision(
        &self,
        inputs: &crate::provision::ProvisionInputs<'_>,
        mode: crate::provision::ProvisionMode,
    ) -> fbuild_core::Result<Vec<crate::provision::ProvisionedPackage>> {
        use crate::provision::{PackageKind, provision_package};
        use orchestrator::SamCore;
        let (toolchain, core) =
            orchestrator::sam_packages(inputs.project_dir, Some(inputs.env_config), inputs.board);
        let mut rows = vec![provision_package(PackageKind::Toolchain, &*toolchain, mode).await];
        match core {
            SamCore::Sam(cores) => {
                rows.push(provision_package(PackageKind::Framework, &cores, mode).await);
            }
            SamCore::Samd {
                cores,
                cmsis,
                cmsis_atmel,
            } => {
                rows.push(provision_package(PackageKind::Framework, &cores, mode).await);
                rows.push(provision_package(PackageKind::Framework, &cmsis, mode).await);
                rows.push(provision_package(PackageKind::Framework, &cmsis_atmel, mode).await);
            }
            SamCore::ClearCore { cores, cmsis } => {
                rows.push(provision_package(PackageKind::Framework, &cores, mode).await);
                rows.push(provision_package(PackageKind::Framework, &cmsis, mode).await);
            }
        }
        Ok(rows)
    }

    fn default_board_id(&self) -> &str {
        "due"
    }
}
