//! Build orchestration facade for all supported platforms.
//!
//! FastLED/fbuild#1008 compile-parallelism split: the shared engine lives in
//! `fbuild-build-engine`; each platform family lives in its own crate; this
//! facade re-exports everything at its original paths and owns the one piece of
//! code that must see both the `PlatformSupport` trait and every platform impl
//! — the `get_platform_support()` factory. Consumers (`fbuild-cli`,
//! `fbuild-daemon`, …) keep depending on `fbuild-build` with unchanged paths:
//! `fbuild_build::pipeline::…`, `fbuild_build::esp32::…`,
//! `fbuild_build::PlatformSupport`, `fbuild_build::get_platform_support`.

// Re-export the entire engine surface at the facade root so every existing
// `fbuild_build::<engine_item>` path (and every in-crate `crate::<engine_item>`
// reference from the platform modules below) keeps resolving unchanged.
pub use fbuild_build_engine::*;

// `compile_many` is the multi-sketch driver: it dispatches per-platform via
// `get_orchestrator()`, so it lives in the facade (not the engine) alongside
// the platform factory. It references engine modules as `crate::<mod>`, which
// resolve through the `pub use fbuild_build_engine::*` re-export above.
pub mod compile_many;

// Platform orchestrators, now in per-family crates that compile in parallel on
// top of the engine (FastLED/fbuild#1008 A2). Re-exported here so every
// existing `fbuild_build::esp32::…` / `fbuild_build::teensy::…` path — and the
// `get_platform_support()` factory below — keeps resolving unchanged.
pub use fbuild_build_arm::{
    apollo3, generic_arm, nrf52, nxplpc, renesas, rp2040, sam, silabs, stm32, teensy,
};
pub use fbuild_build_esp::{esp32, esp8266};
pub use fbuild_build_mcu::{avr, ch32v};

use std::path::Path;

use fbuild_core::{Platform, Result};

/// Look up platform support for a given platform.
///
/// This is the ONE piece of code that must see both the [`PlatformSupport`]
/// trait (from the engine) and every platform implementation, so it lives in
/// the facade. Returns `Err` for platforms without a native orchestrator.
pub fn get_platform_support(platform: Platform) -> Result<Box<dyn PlatformSupport>> {
    match platform {
        Platform::Apollo3 => Ok(Box::new(apollo3::Apollo3PlatformSupport)),
        Platform::AtmelAvr | Platform::AtmelMegaAvr => Ok(Box::new(avr::AvrPlatformSupport)),
        Platform::Espressif32 => Ok(Box::new(esp32::Esp32PlatformSupport)),
        Platform::Espressif8266 => Ok(Box::new(esp8266::Esp8266PlatformSupport)),
        Platform::Teensy => Ok(Box::new(teensy::TeensyPlatformSupport)),
        Platform::Ststm32 => Ok(Box::new(stm32::Stm32PlatformSupport)),
        Platform::RaspberryPi => Ok(Box::new(rp2040::Rp2040PlatformSupport)),
        Platform::NordicNrf52 => Ok(Box::new(nrf52::Nrf52PlatformSupport)),
        Platform::NxpLpc => Ok(Box::new(nxplpc::NxpLpcPlatformSupport)),
        Platform::AtmelSam => Ok(Box::new(sam::SamPlatformSupport)),
        Platform::RenesasRa => Ok(Box::new(renesas::RenesasPlatformSupport)),
        Platform::SiliconLabs => Ok(Box::new(silabs::SilabsPlatformSupport)),
        Platform::Ch32v => Ok(Box::new(ch32v::Ch32vPlatformSupport)),
        _ => Err(fbuild_core::FbuildError::BuildFailed(format!(
            "native orchestrator for {:?} not yet implemented — use --platformio flag for this platform",
            platform
        ))),
    }
}

/// Select the appropriate orchestrator for a platform.
pub fn get_orchestrator(platform: Platform) -> Result<Box<dyn BuildOrchestrator>> {
    get_platform_support(platform).map(|s| s.create_orchestrator())
}

/// Install platform-specific dependencies (toolchain, framework).
pub async fn install_platform_deps(platform: Platform, project_dir: &Path) -> Result<()> {
    get_platform_support(platform)?
        .install_deps(project_dir)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_orchestrator_atmelmegaavr() {
        let orch = get_orchestrator(Platform::AtmelMegaAvr).unwrap();
        assert_eq!(orch.platform(), Platform::AtmelAvr);
    }

    #[test]
    fn test_get_orchestrator_esp8266() {
        let orch = get_orchestrator(Platform::Espressif8266).unwrap();
        assert_eq!(orch.platform(), Platform::Espressif8266);
    }
}
