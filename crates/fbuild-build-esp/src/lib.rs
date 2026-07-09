//! Platform build orchestrators (esp family) — FastLED/fbuild#1008.
//!
//! Re-exports the whole `fbuild-build-engine` surface at the crate root so the
//! platform modules' `crate::<engine_mod>` paths resolve unchanged, then declares
//! the platform modules. Re-exported by the `fbuild-build` facade.
pub use fbuild_build_engine::*;

pub mod esp32;
pub mod esp8266;
