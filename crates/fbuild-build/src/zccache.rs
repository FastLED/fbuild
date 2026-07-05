//! Embedded-mode zccache integration.
//!
//! [FastLED/fbuild#789](https://github.com/FastLED/fbuild/issues/789)
//! Phase 4 stage 2 (#800): the wrapper-binary path is gone. The
//! managed `zccache` binary download, `find_zccache` resolution,
//! `wrap_args` command-line rewriting, `ensure_running` /
//! `stop` daemon lifecycle, and the protocol-mismatch retry
//! recovery are all deleted. Per-compile dispatch and per-watch
//! fingerprint checks always go through
//! [`crate::zccache_embedded`].
//!
//! Closes FastLED/fbuild#32 — the rationale for the intentionally-
//! detached `zccache start` spawn was that the wrapper daemon had
//! to outlive `fbuild-daemon`. Embedded mode means the cache IS
//! `fbuild-daemon`, so there's nothing to detach.
//!
//! This module retains:
//! - [`FingerprintWatch`] / [`FingerprintCheck`] — the public types
//!   build-fingerprint consumers pass in/out.
//! - [`check_fingerprint`] / [`mark_fingerprint_success`] — thin
//!   wrappers around the embedded fingerprint API.
//! - Path-normalization helpers ([`compile_cwd_from_output`],
//!   [`path_arg_for_compile_cwd`], [`normalize_flags_for_compile_cwd`])
//!   — still used by `compile_source` to keep cache keys
//!   workspace-relative.

use std::path::PathBuf;

use fbuild_core::{FbuildError, Result};

// Workspace-relativization helpers were moved to `fbuild_core::path` so
// `fbuild-packages` can share them for library compiles (FastLED/fbuild#952).
// Re-exported here so existing `crate::zccache::…` call sites keep working
// unchanged.
pub use fbuild_core::path::{
    compile_cwd_from_output, normalize_flags_for_compile_cwd, path_arg_for_compile_cwd,
};

/// A persistent zccache fingerprint watch.
#[derive(Debug, Clone)]
pub struct FingerprintWatch {
    pub cache_file: PathBuf,
    pub root: PathBuf,
    pub extensions: Vec<String>,
    pub excludes: Vec<String>,
}

/// Result of a zccache fingerprint check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FingerprintCheck {
    Changed,
    Unchanged,
}

/// Ask the embedded fingerprint engine whether the watched root
/// changed since the last successful mark.
///
/// Drives [`crate::zccache_embedded::check_fingerprint_embedded`]
/// directly. The wrapper-binary path was deleted in #800.
pub fn check_fingerprint(watch: &FingerprintWatch) -> Result<FingerprintCheck> {
    use crate::zccache_embedded::EmbeddedFingerprintCheck;
    match crate::zccache_embedded::check_fingerprint_embedded(
        &watch.cache_file,
        &watch.root,
        &watch.extensions,
        &watch.excludes,
    ) {
        Ok(EmbeddedFingerprintCheck::Changed) => Ok(FingerprintCheck::Changed),
        Ok(EmbeddedFingerprintCheck::Unchanged) => Ok(FingerprintCheck::Unchanged),
        Err(err) => Err(FbuildError::BuildFailed(format!(
            "embedded fingerprint check failed for {}: {err}",
            watch.root.display(),
        ))),
    }
}

/// Mark a previously checked watch as successful.
///
/// Drives [`crate::zccache_embedded::mark_fingerprint_success_embedded`]
/// directly.
pub fn mark_fingerprint_success(watch: &FingerprintWatch) -> Result<()> {
    crate::zccache_embedded::mark_fingerprint_success_embedded(&watch.cache_file).map_err(|err| {
        FbuildError::BuildFailed(format!(
            "embedded fingerprint mark-success failed for {}: {err}",
            watch.root.display(),
        ))
    })
}
