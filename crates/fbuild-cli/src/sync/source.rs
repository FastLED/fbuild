//! Classification of PlatformIO `lib_deps` entries into source types.
//!
//! Moved to `fbuild_config::lib_source` (FastLED/fbuild#1076 Phase 2) so
//! `fbuild-daemon`'s read-only `/api/ide/libraries` endpoint can reuse the
//! exact same classification logic as `fbuild sync` — `fbuild-daemon` can't
//! depend on `fbuild-cli` (the dependency runs the other way: `fbuild-cli`
//! is a thin HTTP client of the daemon), but both already depend on
//! `fbuild-config`. This module is now just a re-export so existing
//! `super::source::*` imports inside `fbuild-cli::sync` keep working
//! unchanged.

pub use fbuild_config::lib_source::*;
