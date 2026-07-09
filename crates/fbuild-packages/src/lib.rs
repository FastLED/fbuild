//! Package-management facade (FastLED/fbuild#1008 Phase B compile-parallelism
//! split). The fetch primitives + base traits live in `fbuild-packages-fetch`;
//! `library` and `toolchain`/`lnk` live in `fbuild-library` / `fbuild-toolchain`
//! and compile in parallel on top of fetch. This facade re-exports all three at
//! their original paths so every `fbuild_packages::…` path — and every consumer
//! (fbuild-build*, fbuild-deploy, fbuild-cli, fbuild-daemon, …) — is unchanged.

pub use fbuild_library::library;
pub use fbuild_packages_fetch::*;
pub use fbuild_toolchain::{lnk, toolchain};
pub use fbuild_toolchain::{ExtractMode, LnkFile};
