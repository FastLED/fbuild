//! Library resolution + management for fbuild (FastLED/fbuild#1008 Phase B).
//!
//! Re-exports the `fbuild-packages-fetch` base (primitives + Package traits) at
//! the crate root so `library`'s `crate::Package` / `crate::cache::…` paths
//! resolve unchanged. Re-exported by the `fbuild-packages` facade.
pub use fbuild_packages_fetch::*;

pub mod library;
