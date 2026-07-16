//! Platform-agnostic build engine shared by every fbuild platform crate.
//!
//! Extracted from `fbuild-build` for compile parallelism (FastLED/fbuild#1008):
//! this crate holds the shared engine modules (pipeline, compiler, linker,
//! analyzers, framework/library plumbing, …), the `PlatformSupport` /
//! `BuildOrchestrator` trait definitions the per-platform crates implement, and
//! the `BuildParams` / `BuildResult` types those orchestrators exchange.
//!
//! It NEVER references a platform module (ENGINE→PLATFORM = 0), so the
//! per-platform crates compile in parallel on top of it. The `fbuild-build`
//! facade re-exports everything here at its original paths.

pub mod arduino_props;
pub mod build_fingerprint;
pub mod build_info;
pub mod build_output;
pub mod compile_backend;
pub mod compile_database;
pub mod compiler;
pub mod eh_frame_policy;
pub mod eh_frame_policy_compute;
pub mod flag_overlay;
pub mod framework_core_cache;
pub mod framework_libs;
pub mod linker;
pub mod mcu_config;
pub mod package_override;
pub mod parallel;
pub mod perf_log;
pub mod pipeline;
pub mod resolution;
pub mod script_runtime;
pub mod shrink;
pub mod source_scanner;
pub mod symbol_analyzer;
pub mod zccache;
pub mod zccache_embedded;

pub use source_scanner::SourceScanner;

use std::path::{Path, PathBuf};

use fbuild_core::{BuildProfile, Platform, Result, SizeInfo};

/// Trait for platform-specific build support.
///
/// Each platform crate implements this to provide orchestrator creation,
/// dependency installation, and configuration. The `fbuild-build` facade's
/// `get_platform_support()` factory maps a [`Platform`] to the right impl.
///
/// FastLED/fbuild#820 (Phase B of #813): `install_deps` is `async` so
/// per-platform impls can `.await` `fbuild_packages::Package::ensure_installed`.
#[async_trait::async_trait]
pub trait PlatformSupport: Send + Sync {
    /// Create the build orchestrator for this platform.
    fn create_orchestrator(&self) -> Box<dyn BuildOrchestrator>;

    /// Install platform-specific dependencies (toolchain, framework).
    async fn install_deps(&self, project_dir: &Path) -> Result<()>;

    /// Default board ID used as fallback when none is specified.
    fn default_board_id(&self) -> &str;
}

/// Warn if user has debug flags (`-g`, `-g1`, `-g2`, `-g3`) in global `build_flags`.
///
/// These flags apply to ALL compilation (sketch, core, and libraries), not just the
/// user's own code.  Compiling the framework with debug info inflates object files ~30x
/// and massively slows linking.  Suggest moving the flag to `build_src_flags`.
pub fn warn_debug_build_flags(user_build_flags: &[String]) {
    let debug_flags: Vec<&str> = user_build_flags
        .iter()
        .filter(|f| {
            let f = f.as_str();
            if f == "-g" {
                return true;
            }
            if f.starts_with("-g") && !f.starts_with("-gnone") && f != "-g0" {
                if let Some(c) = f.chars().nth(2) {
                    return matches!(c, '0'..='3' | 'g' | 'd');
                }
            }
            false
        })
        .map(|s| s.as_str())
        .collect();

    if debug_flags.is_empty() {
        return;
    }

    let flag_str = debug_flags.join(" ");
    tracing::warn!(
        "build_flags contains '{}' which applies to ALL files (sketch, core, libraries).\n  \
         Compiling the framework with debug info inflates object files ~30x and massively slows linking.\n  \
         Recommendation: move '{}' from build_flags to build_src_flags so it only applies to your sketch code,\n  \
         or replace it with '-g0' in build_flags to disable debug info for all files.",
        flag_str,
        flag_str,
    );
}

/// Result of a successful build.
pub struct BuildResult {
    pub success: bool,
    pub firmware_path: Option<PathBuf>,
    pub elf_path: Option<PathBuf>,
    pub size_info: Option<SizeInfo>,
    pub symbol_map: Option<fbuild_core::SymbolMap>,
    pub build_time_secs: f64,
    pub message: String,
    /// Path to the generated `compile_commands.json`, if any.
    pub compile_database_path: Option<PathBuf>,
    /// Accumulated build output (headers, compilation steps, warnings, etc.).
    pub build_log: fbuild_core::BuildLog,
}

/// Input parameters for a build.
pub struct BuildParams {
    pub project_dir: PathBuf,
    pub env_name: String,
    /// Remove the matching reusable framework caches before building. Implies
    /// `clean` at the CLI boundary; normal `clean` only removes project output.
    pub clean_all: bool,
    pub clean: bool,
    pub profile: BuildProfile,
    pub build_dir: PathBuf,
    pub verbose: bool,
    pub jobs: Option<usize>,
    pub generate_compiledb: bool,
    /// When true, skip compilation/linking and only generate `compile_commands.json`.
    /// Used by IWYU and clang-tidy to avoid building framework core files.
    pub compiledb_only: bool,
    /// Optional sender for streaming build log lines in real-time.
    ///
    /// Uses `fbuild_core::channel::UnboundedSender` (the workspace bridge over
    /// `tokio::sync::mpsc::UnboundedSender`) so the orchestrator (running on a
    /// tokio runtime) and the WebSocket forwarder can share one channel
    /// without a sync→async bridge — `UnboundedSender::send` is sync and safe
    /// to call from blocking code, while the receive side is awaited from the
    /// async daemon handler (fbuild#818). Routed via `fbuild_core::channel`
    /// to satisfy the workspace `ban_tokio_mpsc_direct_import` dylint
    /// (fbuild#844).
    pub log_sender: Option<fbuild_core::channel::UnboundedSender<String>>,
    /// When true, run symbol-level memory analysis after linking.
    pub symbol_analysis: bool,
    /// Optional path to write the symbol analysis report to.
    /// When set, the report is written to this path instead of the build artifacts directory.
    pub symbol_analysis_path: Option<std::path::PathBuf>,
    /// Disable elapsed-time prefix on build output lines.
    pub no_timestamp: bool,
    /// Override for the source directory (PLATFORMIO_SRC_DIR).
    /// When set, takes precedence over INI config and env var.
    pub src_dir: Option<String>,
    /// `PLATFORMIO_*` env var overrides forwarded from the CLI caller.
    ///
    /// The daemon does not inherit caller env vars, so all `PLATFORMIO_*`
    /// config flows through this map. Empty when no overrides apply.
    pub pio_env: std::collections::BTreeMap<String, String>,
    /// Additional build flags injected by the caller for one-off build modes
    /// such as QEMU emulation. These are appended after platformio.ini
    /// `build_flags`, so they can intentionally override board/user defaults.
    pub extra_build_flags: Vec<String>,
    /// Optional daemon-scoped memo for the warm-build fingerprint
    /// `hash_watch_set_stamps` walk. When supplied, the orchestrator
    /// short-circuits the walk on a fresh cache hit — the dominant
    /// non-trivial cost on warm rebuilds of large projects (see
    /// `docs/PERF_WARM_BUILD.md`). `None` from the CLI / tests means
    /// the orchestrator falls back to walking on every call, which is
    /// the pre-existing behaviour.
    pub watch_set_cache: Option<std::sync::Arc<dyn build_fingerprint::WatchSetStampCache>>,
    /// When true, append `-Wl,--noinhibit-exec` to the linker command line so
    /// GNU `ld` writes `firmware.elf` even when a memory region overflows, and
    /// treat post-link "failure" as success-with-warning when an ELF was
    /// actually emitted. This unblocks per-symbol bloat analysis on
    /// over-budget builds. See FastLED/fbuild#594.
    pub bloat_analysis: bool,
}

/// Trait for platform-specific build orchestrators.
///
/// FastLED/fbuild#820 (Phase B of #813): `build` is `async` so per-platform
/// orchestrators can `.await` toolchain install, subprocess invocation,
/// and per-TU zccache dispatch directly instead of `Handle::block_on`-ing
/// from a sync entry point.
#[async_trait::async_trait]
pub trait BuildOrchestrator: Send + Sync {
    fn platform(&self) -> Platform;
    async fn build(&self, params: &BuildParams) -> Result<BuildResult>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warn_debug_build_flags_detects_g3() {
        warn_debug_build_flags(&["-g3".to_string()]);
    }

    #[test]
    fn warn_debug_build_flags_detects_bare_g() {
        warn_debug_build_flags(&["-g".to_string()]);
    }

    #[test]
    fn warn_debug_build_flags_ignores_g0() {
        warn_debug_build_flags(&["-g0".to_string()]);
    }

    #[test]
    fn warn_debug_build_flags_ignores_unrelated() {
        warn_debug_build_flags(&["-O2".to_string(), "-Wall".to_string()]);
    }
}
