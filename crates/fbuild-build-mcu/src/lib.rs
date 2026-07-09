//! Platform build orchestrators (mcu family) — FastLED/fbuild#1008.
//!
//! Re-exports the whole `fbuild-build-engine` surface at the crate root so the
//! platform modules' `crate::<engine_mod>` paths resolve unchanged, then declares
//! the platform modules. Re-exported by the `fbuild-build` facade.
pub use fbuild_build_engine::*;

pub mod avr;
pub mod ch32v;
