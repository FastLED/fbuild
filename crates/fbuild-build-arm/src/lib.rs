//! Platform build orchestrators (arm family) — FastLED/fbuild#1008.
//!
//! Re-exports the whole `fbuild-build-engine` surface at the crate root so the
//! platform modules' `crate::<engine_mod>` paths resolve unchanged, then declares
//! the platform modules. Re-exported by the `fbuild-build` facade.
pub use fbuild_build_engine::*;

pub mod apollo3;
pub mod generic_arm;
pub mod nrf52;
pub mod nxplpc;
pub mod renesas;
pub mod rp2040;
pub mod sam;
pub mod silabs;
pub mod stm32;
pub mod teensy;
