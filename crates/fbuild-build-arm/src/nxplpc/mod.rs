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

use std::path::Path;

use fbuild_core::Result;

/// NXP LPC8xx platform support.
pub struct NxpLpcPlatformSupport;

#[async_trait::async_trait]
impl crate::PlatformSupport for NxpLpcPlatformSupport {
    fn create_orchestrator(&self) -> Box<dyn crate::BuildOrchestrator> {
        orchestrator::create()
    }

    async fn install_deps(&self, project_dir: &Path) -> Result<()> {
        // ARM GCC is the right toolchain for Cortex-M0+ bare metal.
        // Pre-install it (+ CMSIS + the Arduino core framework) so the
        // orchestrator can `ensure_installed` cheaply.
        use fbuild_packages::Package;
        let tc = fbuild_packages::toolchain::ArmToolchain::new(project_dir);
        Package::ensure_installed(&tc).await?;
        let cmsis = fbuild_packages::library::CmsisFramework::new(project_dir);
        Package::ensure_installed(&cmsis).await?;
        let core = fbuild_packages::library::ArduinoCoreLpc8xx::new(project_dir);
        Package::ensure_installed(&core).await?;
        tracing::info!("ARM GCC toolchain + ArduinoCore-LPC8xx installed for NXP LPC8xx");
        Ok(())
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
