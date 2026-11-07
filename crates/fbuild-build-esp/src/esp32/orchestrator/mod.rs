//! ESP32 build orchestrator — wires together config, packages, compiler, linker.
//!
//! Build phases:
//! 1. Parse platformio.ini
//! 2. Load board config (esp32dev/esp32c6/etc.)
//! 3. Load MCU config from embedded JSON
//! 4. Ensure ESP32 platform (pioarduino)
//! 5. Resolve + ensure ESP32 toolchain via metadata
//! 6. Ensure ESP32 framework (Arduino core + ESP-IDF SDK libs)
//! 7. Setup build directories
//! 8. Collect include paths: core + variant + SDK (305+) + user src
//! 9. Download + compile library dependencies
//! 10. Scan sources (sketch + core)
//! 11. Compile core sources
//! 12. Compile sketch sources
//! 13. Link (with linker scripts + SDK libs + library archives)
//! 14. Convert to .bin
//! 15. Copy bootloader.bin + partitions.bin
//! 16. Size reporting
//!
//! The orchestrator is split across sibling files in this directory to keep
//! each one under the 1000-LOC gate. `build.rs` contains the top-level
//! `impl BuildOrchestrator`; everything else is helper modules.

mod boot_artifacts;
mod boot_artifacts_cache;
mod build;
mod cdc;
mod compile_phases;
pub mod disk_requirements;
mod embed;
mod embed_stage;
mod fingerprint;
mod framework_library_cache;
mod framework_libs;
mod helpers;
mod idf;
mod job_pool;
mod local_libs;
mod packages;
mod prefix_map;
mod shrink_link;

pub(crate) use packages::downloadable_lib_deps;
pub use packages::resolve_deploy_esptool;

#[cfg(test)]
mod tests;

/// ESP32 platform build orchestrator.
pub struct Esp32Orchestrator;

#[async_trait::async_trait]
impl crate::BuildOrchestrator for Esp32Orchestrator {
    fn platform(&self) -> fbuild_core::Platform {
        fbuild_core::Platform::Espressif32
    }

    async fn build(&self, params: &crate::BuildParams) -> fbuild_core::Result<crate::BuildResult> {
        let config = fbuild_config::PlatformIOConfig::from_path_with_overrides(
            &params.project_dir.join("platformio.ini"),
            fbuild_config::PioEnvOverrides::from_map(params.pio_env.clone()),
        )?;
        if idf::requested(&config, &params.env_name)? {
            return idf::build(params, &config).await;
        }
        Box::pin(self.build_arduino(params)).await
    }
}

pub use cdc::{cdc_on_boot_enabled, create, is_esp32_project, warn_if_cdc_on_boot};

/// Installing Arduino packages for an IDF environment would report the wrong stack.
pub(crate) async fn provision_esp32(
    inputs: &crate::provision::ProvisionInputs<'_>,
    mode: crate::provision::ProvisionMode,
) -> fbuild_core::Result<Vec<crate::provision::ProvisionedPackage>> {
    let frameworks = helpers::requested_frameworks(
        inputs
            .env_config
            .get("framework")
            .map_or("", String::as_str),
    );
    if frameworks.espidf && !frameworks.arduino {
        return Err(fbuild_core::FbuildError::ConfigError("fbuild install for framework=espidf is not yet supported; fbuild build provisions its IDF packages automatically".into()));
    }
    packages::provision_esp32(inputs, mode).await
}
