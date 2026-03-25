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
