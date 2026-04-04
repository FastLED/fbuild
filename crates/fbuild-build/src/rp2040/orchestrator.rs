//! RP2040/RP2350 build orchestrator.

use std::path::Path;

use fbuild_core::{Platform, Result};

use crate::BuildOrchestrator;

/// RP2040 platform build orchestrator.
pub struct Rp2040Orchestrator;

impl BuildOrchestrator for Rp2040Orchestrator {
    fn platform(&self) -> Platform {
        Platform::RaspberryPi
    }

    fn build(&self, _params: &crate::BuildParams) -> Result<crate::BuildResult> {
        Err(fbuild_core::FbuildError::BuildFailed(
            "RP2040 platform build not yet available: Rp2040Cores package not wired up".into(),
        ))
    }
}

/// Create an RP2040 orchestrator.
pub fn create() -> Box<dyn BuildOrchestrator> {
    Box::new(Rp2040Orchestrator)
}

/// Check if a project is configured for RP2040.
pub fn is_rp2040_project(project_dir: &Path, env_name: &str) -> bool {
    crate::pipeline::is_platform_project(project_dir, env_name, fbuild_core::Platform::RaspberryPi)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rp2040_orchestrator_platform() {
        let orch = Rp2040Orchestrator;
        assert_eq!(orch.platform(), Platform::RaspberryPi);
    }
}
