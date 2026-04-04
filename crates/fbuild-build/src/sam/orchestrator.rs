//! SAM build orchestrator — wires together config, packages, compiler, linker.
//!
//! Build phases:
//! 1. Parse platformio.ini
//! 2. Load board config (due, etc.)
//! 3. Ensure ARM GCC toolchain
//! 4. Ensure SAM cores (via `fbuild_packages::library::SamCores` — not yet implemented)
//! 5. Setup build directories
//! 6. Scan source files
//! 7. Compile core sources
//! 8. Compile sketch sources
//! 9. Link (with linker script)
//! 10. Convert to binary + report size

use std::path::Path;

use fbuild_core::{Platform, Result};

use crate::BuildOrchestrator;

// These imports will be used once SamCores is available in fbuild_packages:
// use std::time::Instant;
// use crate::compile_database::TargetArchitecture;
// use crate::pipeline;
// use crate::{BuildParams, BuildResult, SourceScanner};
// use super::sam_compiler::SamCompiler;
// use super::sam_linker::SamLinker;

/// SAM platform build orchestrator.
pub struct SamOrchestrator;

impl BuildOrchestrator for SamOrchestrator {
    fn platform(&self) -> Platform {
        Platform::AtmelSam
    }

    fn build(&self, _params: &crate::BuildParams) -> Result<crate::BuildResult> {
        // TODO: Enable once fbuild_packages::library::SamCores is implemented.
        //
        // The full build pipeline will:
        // 1. Parse config via pipeline::BuildContext::new()
        // 2. Install ARM GCC toolchain via ArmToolchain
        // 3. Install SAM cores via SamCores
        // 4. Scan sources via SourceScanner
        // 5. Compile with SamCompiler (see sam_compiler.rs)
        // 6. Link with SamLinker (see sam_linker.rs)
        // 7. Run pipeline::run_sequential_build() with TargetArchitecture::Arm
        Err(fbuild_core::FbuildError::BuildFailed(
            "SAM platform build not yet available: SamCores package not implemented".into(),
        ))
    }
}

/// Create a SAM orchestrator (convenience for get_orchestrator dispatch).
pub fn create() -> Box<dyn BuildOrchestrator> {
    Box::new(SamOrchestrator)
}

/// Check if a project is configured for SAM by reading its platformio.ini.
pub fn is_sam_project(project_dir: &Path, env_name: &str) -> bool {
    crate::pipeline::is_platform_project(project_dir, env_name, fbuild_core::Platform::AtmelSam)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sam_orchestrator_platform() {
        let orch = SamOrchestrator;
        assert_eq!(orch.platform(), Platform::AtmelSam);
    }
}
