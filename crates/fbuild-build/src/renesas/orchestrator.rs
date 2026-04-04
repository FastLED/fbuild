//! Renesas RA build orchestrator — wires together config, packages, compiler, linker.
//!
//! Build phases:
//! 1. Parse platformio.ini
//! 2. Load board config (uno_r4_wifi, etc.)
//! 3. Ensure ARM GCC toolchain
//! 4. Ensure Renesas cores (via `fbuild_packages::library::RenesasCores` — not yet implemented)
//! 5. Setup build directories
//! 6. Scan source files
//! 7. Compile core sources
//! 8. Compile sketch sources
//! 9. Link (with linker script)
//! 10. Convert to binary + report size

use std::path::Path;

use fbuild_core::{Platform, Result};

use crate::BuildOrchestrator;

// These imports will be used once RenesasCores is available in fbuild_packages:
// use std::time::Instant;
// use crate::compile_database::TargetArchitecture;
// use crate::pipeline;
// use crate::{BuildParams, BuildResult, SourceScanner};
// use super::renesas_compiler::RenesasCompiler;
// use super::renesas_linker::RenesasLinker;

/// Renesas RA platform build orchestrator.
pub struct RenesasOrchestrator;

impl BuildOrchestrator for RenesasOrchestrator {
    fn platform(&self) -> Platform {
        Platform::RenesasRa
    }

    fn build(&self, _params: &crate::BuildParams) -> Result<crate::BuildResult> {
        // TODO: Enable once fbuild_packages::library::RenesasCores is implemented.
        //
        // The full build pipeline will:
        // 1. Parse config via pipeline::BuildContext::new()
        // 2. Install ARM GCC toolchain via ArmToolchain
        // 3. Install Renesas cores via RenesasCores
        // 4. Scan sources via SourceScanner
        // 5. Compile with RenesasCompiler (see renesas_compiler.rs)
        // 6. Link with RenesasLinker (see renesas_linker.rs)
        // 7. Run pipeline::run_sequential_build() with TargetArchitecture::Arm
        Err(fbuild_core::FbuildError::BuildFailed(
            "Renesas RA platform build not yet available: RenesasCores package not implemented"
                .into(),
        ))
    }
}

/// Create a Renesas orchestrator (convenience for get_orchestrator dispatch).
pub fn create() -> Box<dyn BuildOrchestrator> {
    Box::new(RenesasOrchestrator)
}

/// Check if a project is configured for Renesas by reading its platformio.ini.
pub fn is_renesas_project(project_dir: &Path, env_name: &str) -> bool {
    crate::pipeline::is_platform_project(project_dir, env_name, fbuild_core::Platform::RenesasRa)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_renesas_orchestrator_platform() {
        let orch = RenesasOrchestrator;
        assert_eq!(orch.platform(), Platform::RenesasRa);
    }
}
