//! Toolchain resolution + `.lnk` blob handling for fbuild (FastLED/fbuild#1008 B).
//!
//! Re-exports the `fbuild-packages-fetch` base at the crate root so `toolchain`
//! and `lnk` keep resolving `crate::Package` / `crate::cache::…` etc.
//! Re-exported by the `fbuild-packages` facade.
pub use fbuild_packages_fetch::*;

pub mod lnk;
pub mod toolchain;

pub use lnk::{ExtractMode, LnkFile};
