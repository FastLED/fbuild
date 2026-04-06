//! Teensy build orchestrator — wires together config, packages, compiler, linker.
//!
//! Build phases:
//! 1. Parse platformio.ini
//! 2. Load board config (teensy40/teensy41)
//! 3. Ensure ARM GCC toolchain
//! 4. Ensure Teensy cores
//! 5. Setup build directories
//! 6. Scan source files
//! 7. Compile core sources (teensy4/*.c, *.cpp)
//! 8. Compile sketch sources
//! 9. Link (with linker script from teensy4/)
//! 10. Convert to hex + report size

use std::path::Path;
use std::time::Instant;

use fbuild_core::{Platform, Result};

use crate::compile_database::TargetArchitecture;
use crate::pipeline;
use crate::{BuildOrchestrator, BuildParams, BuildResult, SourceScanner};

use super::teensy_compiler::TeensyCompiler;
use super::teensy_linker::TeensyLinker;

/// Teensy platform build orchestrator.
pub struct TeensyOrchestrator;

impl BuildOrchestrator for TeensyOrchestrator {
    fn platform(&self) -> Platform {
        Platform::Teensy
    }

    fn build(&self, params: &BuildParams) -> Result<BuildResult> {
        let start = Instant::now();

        // 1-2. Parse config, load board, setup build dirs, resolve src dir, collect flags
        let mut ctx = pipeline::BuildContext::new(
            &params.project_dir,
            &params.env_name,
            params.clean,
            params.profile,
            params.log_sender.clone(),
            params.no_timestamp,
        )?;

        // Need board_id for linker script lookup later
        let env_config = ctx.config.get_env_config(&params.env_name)?;
        let board_id = env_config.get("board").ok_or_else(|| {
            fbuild_core::FbuildError::ConfigError("missing 'board' in environment config".into())
        })?;

        // 3. Ensure ARM GCC toolchain
        let toolchain = fbuild_packages::toolchain::ArmToolchain::new(&params.project_dir);
        let toolchain_dir = fbuild_packages::Package::ensure_installed(&toolchain)?;
        tracing::info!("arm-gcc toolchain at {}", toolchain_dir.display());

        use fbuild_packages::Toolchain;
        pipeline::log_toolchain_version(
            &toolchain.get_gcc_path(),
            "arm-none-eabi-gcc",
            &mut ctx.build_log,
        );

        // 4. Ensure Teensy cores
        let framework = fbuild_packages::library::TeensyCores::new(&params.project_dir);
        let framework_dir = fbuild_packages::Package::ensure_installed(&framework)?;
        tracing::info!("Teensy cores at {}", framework_dir.display());

        // 5. Scan sources (Teensy: no variants, exclude Blink.cc test sketch)
        let core_dir = framework.get_core_dir(&ctx.board.core);

        let scanner = SourceScanner::new(&ctx.src_dir, &ctx.src_build_dir);
        let mut sources = scanner.scan_all(Some(&core_dir), None)?;
        sources
            .core_sources
            .retain(|p| p.file_name().map(|f| f != "Blink.cc").unwrap_or(true));

        tracing::info!(
            "sources: {} sketch, {} core",
            sources.sketch_sources.len(),
            sources.core_sources.len(),
        );

        // 6. Build include dirs + compiler
        let mcu_config =
            super::mcu_config::get_teensy_config_for_mcu(&ctx.board.mcu.to_lowercase())?;
        let mut defines = ctx.board.get_defines();
        defines.extend(mcu_config.defines_map());
        let mut include_dirs = vec![core_dir.clone()];
        include_dirs.push(ctx.src_dir.clone());
        pipeline::discover_project_includes(&params.project_dir, &mut include_dirs);
        // Toolchain sysroot includes (ARM CMSIS headers, etc.)
        include_dirs.extend(toolchain.get_include_dirs());

        let compiler = TeensyCompiler::new(
            toolchain.get_gcc_path(),
            toolchain.get_gxx_path(),
            &ctx.board.mcu,
            &ctx.board.f_cpu,
            defines,
            include_dirs,
            mcu_config.clone(),
            params.profile,
            params.verbose,
        );

        // 7. Create linker (with linker script from board config)
        let linker_scripts = match ctx.board.ldscript.as_deref() {
            Some(name) => crate::linker::LinkerScripts::single(core_dir.clone(), name),
            None => {
                // Fallback: framework's hardcoded lookup for backward compatibility
                let path = framework.get_linker_script(board_id);
                crate::linker::LinkerScripts {
                    search_dirs: vec![],
                    scripts: vec![path.to_string_lossy().to_string()],
                }
            }
        };
        let linker = TeensyLinker::new(
            toolchain.get_gcc_path(),
            toolchain.get_ar_path(),
            toolchain.get_objcopy_path(),
            toolchain.get_size_path(),
            linker_scripts,
            mcu_config,
            params.profile,
            ctx.board.max_flash,
            ctx.board.max_ram,
            params.verbose,
        );

        // 8. Run shared sequential build pipeline
        pipeline::run_sequential_build(
            &compiler,
            &linker,
            ctx,
            params,
            &sources,
            TargetArchitecture::Arm,
            "Teensy",
            start,
        )
    }
}

/// Create a Teensy orchestrator (convenience for get_orchestrator dispatch).
pub fn create() -> Box<dyn BuildOrchestrator> {
    Box::new(TeensyOrchestrator)
}

/// Check if a project is configured for Teensy by reading its platformio.ini.
pub fn is_teensy_project(project_dir: &Path, env_name: &str) -> bool {
    crate::pipeline::is_platform_project(project_dir, env_name, fbuild_core::Platform::Teensy)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_teensy_orchestrator_platform() {
        let orch = TeensyOrchestrator;
        assert_eq!(orch.platform(), Platform::Teensy);
    }

    #[test]
    fn test_is_teensy_project() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:teensy41]\nplatform = teensy\nboard = teensy41\nframework = arduino\n",
        )
        .unwrap();
        assert!(is_teensy_project(tmp.path(), "teensy41"));
        assert!(!is_teensy_project(tmp.path(), "uno"));
    }

    #[test]
    fn test_is_not_teensy_project() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:uno]\nplatform = atmelavr\nboard = uno\nframework = arduino\n",
        )
        .unwrap();
        assert!(!is_teensy_project(tmp.path(), "uno"));
    }
}
