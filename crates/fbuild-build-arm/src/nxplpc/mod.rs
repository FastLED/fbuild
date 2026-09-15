//! NXP LPC8xx (Cortex-M0+) bare-metal build support.
//!
//! - Stage 1 (shipped): board/toolchain wiring, board JSON, dispatch entry.
//! - Stage 2 (shipped): build orchestrator (see [`orchestrator`]).
//! - Stage 3/4 (this module, #479 / #487): the orchestrator vendors the real
//!   Arduino framework [`zackees/ArduinoCore-LPC8xx`](https://github.com/zackees/ArduinoCore-LPC8xx)
//!   via the package downloader (`ArduinoCoreLpc8xx`) â€” framework-owned
//!   `main()`, startup + vector table, wiring, HardwareSerial, SPI, the GCC
//!   linker scripts, and per-board variants. The previously embedded
//!   `arduino_stub/`, device headers, startup `.S`, linker scripts, and
//!   `main.cpp` shim are retired by this consumption.
//!
//! Tracked under #487.

pub mod mcu_config;
pub mod orchestrator;
// `platform_packages` lookup is now shared at the workspace level
// (FastLED/fbuild#681) â€” see `crate::package_override`. The per-platform
// parser introduced in #663 has been folded into
// `fbuild_config::platform_packages` so every orchestrator gets the same
// parser without duplication.

use fbuild_core::Result;

/// NXP LPC8xx platform support.
pub struct NxpLpcPlatformSupport;

#[async_trait::async_trait]
impl crate::PlatformSupport for NxpLpcPlatformSupport {
    fn create_orchestrator(&self) -> Box<dyn crate::BuildOrchestrator> {
        orchestrator::create()
    }

    async fn provision(
        &self,
        inputs: &crate::provision::ProvisionInputs<'_>,
        mode: crate::provision::ProvisionMode,
    ) -> Result<Vec<crate::provision::ProvisionedPackage>> {
        use crate::provision::{PackageKind, provision_package};
        let (toolchain, cmsis, core) =
            orchestrator::nxplpc_packages(inputs.project_dir, Some(inputs.env_config));
        Ok(vec![
            provision_package(PackageKind::Toolchain, &toolchain, mode).await,
            provision_package(PackageKind::Framework, &cmsis, mode).await,
            provision_package(PackageKind::Framework, &core, mode).await,
        ])
    }

    fn default_board_id(&self) -> &str {
        "lpc845"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PlatformSupport;

    #[test]
    fn default_board_is_lpc845() {
        assert_eq!(NxpLpcPlatformSupport.default_board_id(), "lpc845");
    }

    #[test]
    fn creates_nxplpc_orchestrator() {
        let orch = NxpLpcPlatformSupport.create_orchestrator();
        assert_eq!(orch.platform(), fbuild_core::Platform::NxpLpc);
    }
}
