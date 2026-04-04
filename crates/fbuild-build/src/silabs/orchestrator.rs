//! Silicon Labs build orchestrator — wires together config, packages, compiler, linker.
//!
//! Build phases:
//! 1. Parse platformio.ini
//! 2. Load board config (sparkfun_thingplusmatter, etc.)
//! 3. Ensure ARM GCC toolchain
//! 4. Ensure SiLabs cores (via `fbuild_packages::library::SilabsCores` — not yet implemented)
//! 5. Setup build directories
//! 6. Scan source files
//! 7. Compile core sources
//! 8. Compile sketch sources
//! 9. Link (with linker script)
//! 10. Convert to binary + report size

use std::path::Path;

use fbuild_core::{Platform, Result};

use crate::BuildOrchestrator;

// These imports will be used once SilabsCores is available in fbuild_packages:
// use std::time::Instant;
// use crate::compile_database::TargetArchitecture;
// use crate::pipeline;
// use crate::{BuildParams, BuildResult, SourceScanner};
// use super::silabs_compiler::SilabsCompiler;
// use super::silabs_linker::SilabsLinker;

/// Silicon Labs platform build orchestrator.
pub struct SilabsOrchestrator;

impl BuildOrchestrator for SilabsOrchestrator {
    fn platform(&self) -> Platform {
        Platform::SiliconLabs
    }

    fn build(&self, _params: &crate::BuildParams) -> Result<crate::BuildResult> {
        // TODO: Enable once fbuild_packages::library::SilabsCores is implemented.
        //
        // The full build pipeline will:
        // 1. Parse config via pipeline::BuildContext::new()
        // 2. Install ARM GCC toolchain via ArmToolchain
        // 3. Install SiLabs cores via SilabsCores
        // 4. Scan sources via SourceScanner
        // 5. Compile with SilabsCompiler (see silabs_compiler.rs)
        // 6. Link with SilabsLinker (see silabs_linker.rs)
        // 7. Run pipeline::run_sequential_build() with TargetArchitecture::Arm
        Err(fbuild_core::FbuildError::BuildFailed(
            "Silicon Labs platform build not yet available: SilabsCores package not implemented"
                .into(),
        ))
    }
}

/// Create a Silicon Labs orchestrator (convenience for get_orchestrator dispatch).
pub fn create() -> Box<dyn BuildOrchestrator> {
    Box::new(SilabsOrchestrator)
}

/// Check if a project is configured for Silicon Labs by reading its platformio.ini.
pub fn is_silabs_project(project_dir: &Path, env_name: &str) -> bool {
    crate::pipeline::is_platform_project(project_dir, env_name, fbuild_core::Platform::SiliconLabs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_silabs_orchestrator_platform() {
        let orch = SilabsOrchestrator;
        assert_eq!(orch.platform(), Platform::SiliconLabs);
    }
}
