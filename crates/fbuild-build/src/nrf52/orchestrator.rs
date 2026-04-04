//! NRF52 build orchestrator — wires together config, packages, compiler, linker.
//!
//! Build phases:
//! 1. Parse platformio.ini
//! 2. Load board config (nrf52840_dk, etc.)
//! 3. Ensure ARM GCC toolchain
//! 4. Ensure NRF52 cores (via `fbuild_packages::library::Nrf52Cores` — not yet implemented)
//! 5. Setup build directories
//! 6. Scan source files
//! 7. Compile core sources
//! 8. Compile sketch sources
//! 9. Link (with linker script)
//! 10. Convert to hex + report size

use std::path::Path;

use fbuild_core::{Platform, Result};

use crate::BuildOrchestrator;

// These imports will be used once Nrf52Cores is available in fbuild_packages:
// use std::time::Instant;
// use crate::compile_database::TargetArchitecture;
// use crate::pipeline;
// use crate::{BuildParams, BuildResult, SourceScanner};
// use super::nrf52_compiler::Nrf52Compiler;
// use super::nrf52_linker::Nrf52Linker;

/// NRF52 platform build orchestrator.
pub struct Nrf52Orchestrator;

impl BuildOrchestrator for Nrf52Orchestrator {
    fn platform(&self) -> Platform {
        Platform::NordicNrf52
    }

    fn build(&self, _params: &crate::BuildParams) -> Result<crate::BuildResult> {
        // TODO: Enable once fbuild_packages::library::Nrf52Cores is implemented.
        //
        // The full build pipeline will:
        // 1. Parse config via pipeline::BuildContext::new()
        // 2. Install ARM GCC toolchain via ArmToolchain
        // 3. Install NRF52 cores via Nrf52Cores
        // 4. Scan sources via SourceScanner
        // 5. Compile with Nrf52Compiler (see nrf52_compiler.rs)
        // 6. Link with Nrf52Linker (see nrf52_linker.rs)
        // 7. Run pipeline::run_sequential_build() with TargetArchitecture::Arm
        Err(fbuild_core::FbuildError::BuildFailed(
            "NRF52 platform build not yet available: Nrf52Cores package not implemented".into(),
        ))
    }
}

/// Create an NRF52 orchestrator (convenience for get_orchestrator dispatch).
pub fn create() -> Box<dyn BuildOrchestrator> {
    Box::new(Nrf52Orchestrator)
}

/// Check if a project is configured for NRF52 by reading its platformio.ini.
pub fn is_nrf52_project(project_dir: &Path, env_name: &str) -> bool {
    crate::pipeline::is_platform_project(project_dir, env_name, fbuild_core::Platform::NordicNrf52)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nrf52_orchestrator_platform() {
        let orch = Nrf52Orchestrator;
        assert_eq!(orch.platform(), Platform::NordicNrf52);
    }
}
