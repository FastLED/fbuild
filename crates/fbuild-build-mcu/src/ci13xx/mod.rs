//! Native Chipintelli CI1306 compile/link support.
mod orchestrator;
mod packages;
pub use orchestrator::Ci13xxOrchestrator;

pub struct Ci13xxPlatformSupport;

/// Resolve the same installed framework as native builds without downloading.
pub fn installed_framework_root(
    project: &std::path::Path,
) -> fbuild_core::Result<std::path::PathBuf> {
    packages::VendorPackage::framework(project).installed_root()
}

#[async_trait::async_trait]
impl crate::PlatformSupport for Ci13xxPlatformSupport {
    fn default_board_id(&self) -> &str {
        "ci1306"
    }

    fn create_orchestrator(&self) -> Box<dyn crate::BuildOrchestrator> {
        Box::new(Ci13xxOrchestrator)
    }
    async fn provision(
        &self,
        inputs: &crate::provision::ProvisionInputs<'_>,
        mode: crate::provision::ProvisionMode,
    ) -> fbuild_core::Result<Vec<crate::provision::ProvisionedPackage>> {
        orchestrator::validate(inputs.env_config, &inputs.board.mcu)?;
        let toolchain = packages::VendorPackage::toolchain(inputs.project_dir)?;
        let framework = packages::VendorPackage::framework(inputs.project_dir);
        use crate::provision::{PackageKind, provision_package};
        let reports = vec![
            provision_package(PackageKind::Toolchain, &toolchain, mode).await,
            provision_package(PackageKind::Framework, &framework, mode).await,
        ];
        if fbuild_packages::Package::is_installed(&framework) {
            orchestrator::validate_board(inputs.board, &framework.installed_root()?)?;
        }
        Ok(reports)
    }
}
