//! NXP LPC8xx (Cortex-M0+) bare-metal CMSIS build support.
//!
//! Stage 1 of FastLED/FastLED#2836: scaffolds board/toolchain wiring for
//! LPC804 and LPC845. Stage 2 (FastLED C++ driver port) will land separately
//! and unblock the per-platform CI workflows.

pub mod mcu_config;

use std::path::Path;

use fbuild_core::Result;

/// Linker script for LPC804 (32 KB Flash, 4 KB RAM).
///
/// Origins / sizes are taken from the LPC804 datasheet, table 5
/// ("Memory mapping"). Standard Cortex-M0+ section layout.
pub const LPC804_LD: &str = include_str!("assets/lpc804.ld");

/// Linker script for LPC845 (64 KB Flash, 16 KB RAM).
///
/// Origins / sizes are taken from the LPC84x datasheet, section 7.6
/// ("Memory map"). Standard Cortex-M0+ section layout.
pub const LPC845_LD: &str = include_str!("assets/lpc845.ld");

/// Minimal Reset_Handler + vector table for LPC804.
pub const LPC804_STARTUP: &str = include_str!("assets/startup_lpc804.S");

/// Minimal Reset_Handler + vector table for LPC845.
pub const LPC845_STARTUP: &str = include_str!("assets/startup_lpc845.S");

/// NXP LPC8xx platform support.
///
/// Stage 1 ships board/toolchain wiring only. `create_orchestrator()` returns
/// a stub orchestrator that fails fast with a Stage-2-required message; the
/// upstream `_ =>` arm in `crate::get_platform_support` already covers the
/// "not yet implemented" path, but registering NxpLpc here keeps the dispatch
/// table consistent with other platforms and lets Stage 2 plug in without
/// touching `lib.rs` again.
pub struct NxpLpcPlatformSupport;

impl crate::PlatformSupport for NxpLpcPlatformSupport {
    fn create_orchestrator(&self) -> Box<dyn crate::BuildOrchestrator> {
        Box::new(NxpLpcStubOrchestrator)
    }

    fn install_deps(&self, project_dir: &Path) -> Result<()> {
        // ARM GCC is the right toolchain for Cortex-M0+ bare metal.
        // Pre-install it so Stage 2's orchestrator can `ensure_installed` cheaply.
        use fbuild_packages::Package;
        let tc = fbuild_packages::toolchain::ArmToolchain::new(project_dir);
        Package::ensure_installed(&tc)?;
        tracing::info!("ARM GCC toolchain installed for NXP LPC8xx");
        Ok(())
    }

    fn default_board_id(&self) -> &str {
        "lpc845"
    }
}

/// Stage-1 placeholder orchestrator. Returns a clear "Stage 2 required" error
/// so users running `fbuild build` against an LPC8xx board hit a useful
/// message instead of a generic "not yet implemented".
struct NxpLpcStubOrchestrator;

impl crate::BuildOrchestrator for NxpLpcStubOrchestrator {
    fn platform(&self) -> fbuild_core::Platform {
        fbuild_core::Platform::NxpLpc
    }

    fn build(&self, _params: &crate::BuildParams) -> Result<crate::BuildResult> {
        Err(fbuild_core::FbuildError::BuildFailed(
            "NXP LPC8xx build orchestrator is Stage 2 of FastLED/FastLED#2836 \
             and has not landed yet. Stage 1 (this PR) wires only the board \
             definitions, Platform enum entry, linker scripts, and startup \
             stubs. Track FastLED/fbuild for the Stage-2 follow-up."
                .to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linker_scripts_have_expected_memory_regions() {
        assert!(LPC804_LD.contains("FLASH"));
        assert!(LPC804_LD.contains("0x00000000"));
        assert!(LPC804_LD.contains("LENGTH = 32K"));
        assert!(LPC804_LD.contains("0x10000000"));
        assert!(LPC804_LD.contains("LENGTH = 4K"));

        assert!(LPC845_LD.contains("FLASH"));
        assert!(LPC845_LD.contains("LENGTH = 64K"));
        assert!(LPC845_LD.contains("LENGTH = 16K"));
    }

    #[test]
    fn startup_files_define_reset_handler() {
        assert!(LPC804_STARTUP.contains("Reset_Handler"));
        assert!(LPC845_STARTUP.contains("Reset_Handler"));
    }

    #[test]
    fn stub_orchestrator_reports_nxplpc_platform() {
        use crate::BuildOrchestrator;
        let orch = NxpLpcStubOrchestrator;
        assert_eq!(orch.platform(), fbuild_core::Platform::NxpLpc);
    }
}
