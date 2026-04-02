//! Build orchestration for all supported platforms.
//!
//! Each platform has its own orchestrator implementing the `BuildOrchestrator` trait.
//! Orchestrators handle: source scanning, compilation, linking, size reporting.

pub mod avr;
pub mod compile_database;
pub mod compiler;
pub mod esp32;
pub mod linker;
pub mod parallel;
pub mod source_scanner;
pub mod teensy;
pub mod zccache;

pub use source_scanner::SourceScanner;

use fbuild_core::{BuildProfile, Platform, Result, SizeInfo};

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
use std::path::PathBuf;

/// Result of a successful build.
pub struct BuildResult {
    pub success: bool,
    pub hex_path: Option<PathBuf>,
    pub elf_path: Option<PathBuf>,
    pub size_info: Option<SizeInfo>,
    pub build_time_secs: f64,
    pub message: String,
    /// Path to the generated `compile_commands.json`, if any.
    pub compile_database_path: Option<PathBuf>,
}

/// Input parameters for a build.
pub struct BuildParams {
    pub project_dir: PathBuf,
    pub env_name: String,
    pub clean: bool,
    pub profile: BuildProfile,
    pub build_dir: PathBuf,
    pub verbose: bool,
    pub jobs: Option<usize>,
    pub generate_compiledb: bool,
    /// When true, skip compilation/linking and only generate `compile_commands.json`.
    /// Used by IWYU and clang-tidy to avoid building framework core files.
    pub compiledb_only: bool,
}

/// Trait for platform-specific build orchestrators.
pub trait BuildOrchestrator: Send + Sync {
    fn platform(&self) -> Platform;
    fn build(&self, params: &BuildParams) -> Result<BuildResult>;
}

/// Select the appropriate orchestrator for a platform.
pub fn get_orchestrator(platform: Platform) -> Result<Box<dyn BuildOrchestrator>> {
    match platform {
        Platform::AtmelAvr => Ok(avr::orchestrator::create()),
        Platform::Espressif32 => Ok(esp32::orchestrator::create()),
        Platform::Teensy => Ok(teensy::orchestrator::create()),
        _ => Err(fbuild_core::FbuildError::BuildFailed(format!(
            "native orchestrator for {:?} not yet implemented — use --platformio flag for this platform",
            platform
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warn_debug_build_flags_detects_g3() {
        // Should not panic; exercises the detection path.
        warn_debug_build_flags(&["-g3".to_string()]);
    }

    #[test]
    fn warn_debug_build_flags_detects_bare_g() {
        warn_debug_build_flags(&["-g".to_string()]);
    }

    #[test]
    fn warn_debug_build_flags_ignores_g0() {
        // -g0 disables debug info; should not warn.
        warn_debug_build_flags(&["-g0".to_string()]);
    }

    #[test]
    fn warn_debug_build_flags_ignores_unrelated() {
        warn_debug_build_flags(&["-O2".to_string(), "-Wall".to_string()]);
    }
}
