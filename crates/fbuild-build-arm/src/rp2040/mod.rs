//! RP2040/RP2350 platform build support (Raspberry Pi Pico, etc.)

pub mod mcu_config;
pub mod orchestrator;
mod uf2;

pub use orchestrator::Rp2040Orchestrator;

/// RP2040 platform support.
pub struct Rp2040PlatformSupport;

#[async_trait::async_trait]
impl crate::PlatformSupport for Rp2040PlatformSupport {
    fn create_orchestrator(&self) -> Box<dyn crate::BuildOrchestrator> {
        orchestrator::create()
    }

    async fn provision(
        &self,
        inputs: &crate::provision::ProvisionInputs<'_>,
        mode: crate::provision::ProvisionMode,
    ) -> fbuild_core::Result<Vec<crate::provision::ProvisionedPackage>> {
        use crate::provision::{PackageKind, provision_package};
        let (toolchain, picotool, cores) =
            orchestrator::rp2040_packages(inputs.project_dir, Some(inputs.env_config));
        Ok(vec![
            provision_package(PackageKind::Toolchain, &toolchain, mode).await,
            provision_package(PackageKind::Tool, &picotool, mode).await,
            provision_package(PackageKind::Framework, &cores, mode).await,
        ])
    }

    /// arduino-pico bundles libraries (Wire, SPI, ...) that `lib_deps` may
    /// name; the build filters them out before downloading, so provisioning
    /// must too. That needs the installed cores — without them, every entry
    /// is reported.
    fn downloadable_lib_deps(
        &self,
        inputs: &crate::provision::ProvisionInputs<'_>,
        lib_deps: Vec<String>,
    ) -> Vec<String> {
        use fbuild_packages::Package;
        let (_, _, cores) =
            orchestrator::rp2040_packages(inputs.project_dir, Some(inputs.env_config));
        if !cores.is_installed() {
            return lib_deps;
        }
        fbuild_library_select::external_declared_deps(&lib_deps, &cores.get_framework_libraries())
    }

    fn default_board_id(&self) -> &str {
        "rpipico"
    }
}
