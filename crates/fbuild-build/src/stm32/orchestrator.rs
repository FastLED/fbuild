//! STM32 build orchestrator.

use std::path::Path;

use fbuild_core::{Platform, Result};

use crate::BuildOrchestrator;

/// STM32 platform build orchestrator.
pub struct Stm32Orchestrator;

impl BuildOrchestrator for Stm32Orchestrator {
    fn platform(&self) -> Platform {
        Platform::Ststm32
    }

    fn build(&self, _params: &crate::BuildParams) -> Result<crate::BuildResult> {
        Err(fbuild_core::FbuildError::BuildFailed(
            "STM32 platform build not yet available: Stm32Cores package not wired up".into(),
        ))
    }
}

/// Create an STM32 orchestrator.
pub fn create() -> Box<dyn BuildOrchestrator> {
    Box::new(Stm32Orchestrator)
}

/// Check if a project is configured for STM32.
pub fn is_stm32_project(project_dir: &Path, env_name: &str) -> bool {
    crate::pipeline::is_platform_project(project_dir, env_name, fbuild_core::Platform::Ststm32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stm32_orchestrator_platform() {
        let orch = Stm32Orchestrator;
        assert_eq!(orch.platform(), Platform::Ststm32);
    }

    #[test]
    fn test_is_stm32_project() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:bluepill]\nplatform = ststm32\nboard = bluepill_f103c8\nframework = arduino\n",
        )
        .unwrap();
        assert!(is_stm32_project(tmp.path(), "bluepill"));
        assert!(!is_stm32_project(tmp.path(), "uno"));
    }
}
