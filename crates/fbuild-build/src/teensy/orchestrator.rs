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

use crate::compiler::{Compiler, CompilerBase};
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

        // 1. Parse platformio.ini
        let ini_path = params.project_dir.join("platformio.ini");
        let config = fbuild_config::PlatformIOConfig::from_path(&ini_path)?;
        let env_config = config.get_env_config(&params.env_name)?;

        // 2. Load board config
        let board_id = env_config.get("board").ok_or_else(|| {
            fbuild_core::FbuildError::ConfigError("missing 'board' in environment config".into())
        })?;
        let overrides = config.get_board_overrides(&params.env_name)?;
        let board = fbuild_config::BoardConfig::from_board_id(board_id, &overrides)?;

        // 3. Ensure ARM GCC toolchain
        let toolchain = fbuild_packages::toolchain::ArmToolchain::new(&params.project_dir);
        let toolchain_dir = fbuild_packages::Package::ensure_installed(&toolchain)?;
        tracing::info!("arm-gcc toolchain at {}", toolchain_dir.display());

        // 4. Ensure Teensy cores
        let framework = fbuild_packages::library::TeensyCores::new(&params.project_dir);
        let framework_dir = fbuild_packages::Package::ensure_installed(&framework)?;
        tracing::info!("Teensy cores at {}", framework_dir.display());

        // 5. Setup build directories
        let cache = fbuild_packages::Cache::new(&params.project_dir);
        if params.clean {
            cache.clean_build(&params.env_name)?;
        }
        cache.ensure_build_directories(&params.env_name)?;

        let build_dir = cache.get_build_dir(&params.env_name);
        let core_build_dir = cache.get_core_build_dir(&params.env_name);
        let src_build_dir = cache.get_src_build_dir(&params.env_name);

        // 6. Scan sources
        let src_dir = params.project_dir.join(
            config
                .get_src_dir(&params.env_name)?
                .unwrap_or_else(|| "src".to_string()),
        );

        // Teensy cores: core dir is directly under framework (e.g. teensy4/), no variants
        let core_dir = framework.get_core_dir(&board.core);

        let scanner = SourceScanner::new(&src_dir, &src_build_dir);
        // No variant dir for Teensy
        let mut sources = scanner.scan_all(Some(&core_dir), None)?;

        // Exclude Blink.cc — it's a test sketch in the Teensy core that defines setup()/loop()
        sources
            .core_sources
            .retain(|p| p.file_name().map(|f| f != "Blink.cc").unwrap_or(true));

        tracing::info!(
            "sources: {} sketch, {} core",
            sources.sketch_sources.len(),
            sources.core_sources.len(),
        );

        // 7. Compile
        let defines = board.get_defines();
        // Teensy include path is just the core dir (teensy4/), no variants dir
        let mut include_dirs = vec![core_dir.clone()];
        include_dirs.push(src_dir.clone());

        let user_flags = config.get_build_flags(&params.env_name)?;

        use fbuild_packages::Toolchain;
        let compiler = TeensyCompiler::new(
            toolchain.get_gcc_path(),
            toolchain.get_gxx_path(),
            &board.mcu,
            &board.f_cpu,
            defines,
            include_dirs,
            params.verbose,
        );

        // Compile core sources
        let mut core_objects = Vec::new();
        for source in &sources.core_sources {
            let obj = CompilerBase::object_path(source, &core_build_dir);
            if CompilerBase::needs_rebuild(source, &obj) {
                let result = compiler.compile(source, &obj, &user_flags)?;
                if !result.success {
                    return Err(fbuild_core::FbuildError::BuildFailed(format!(
                        "compilation failed for {}:\n{}",
                        source.display(),
                        result.stderr
                    )));
                }
            }
            core_objects.push(obj);
        }

        // Compile sketch sources (with src flags too)
        let src_flags = config.get_build_src_flags(&params.env_name)?;
        let all_src_flags: Vec<String> =
            user_flags.iter().chain(src_flags.iter()).cloned().collect();

        let mut sketch_objects = Vec::new();
        for source in &sources.sketch_sources {
            let obj = CompilerBase::object_path(source, &src_build_dir);
            if CompilerBase::needs_rebuild(source, &obj) {
                let result = compiler.compile(source, &obj, &all_src_flags)?;
                if !result.success {
                    return Err(fbuild_core::FbuildError::BuildFailed(format!(
                        "compilation failed for {}:\n{}",
                        source.display(),
                        result.stderr
                    )));
                }
            }
            sketch_objects.push(obj);
        }

        // 8-9. Link + convert (with linker script)
        let linker_script = framework.get_linker_script(board_id);
        let linker = TeensyLinker::new(
            toolchain.get_gcc_path(),
            toolchain.get_ar_path(),
            toolchain.get_objcopy_path(),
            toolchain.get_size_path(),
            linker_script,
            board.max_flash,
            board.max_ram,
            params.verbose,
        );

        let link_result =
            crate::linker::Linker::link_all(&linker, &sketch_objects, &core_objects, &build_dir)?;

        // 10. Size reporting
        if let Some(ref size) = link_result.size_info {
            tracing::info!(
                "size: text={} data={} bss={} | flash={}/{} ({:.1}%) ram={}/{} ({:.1}%)",
                size.text,
                size.data,
                size.bss,
                size.total_flash,
                size.max_flash.unwrap_or(0),
                size.flash_percent().unwrap_or(0.0),
                size.total_ram,
                size.max_ram.unwrap_or(0),
                size.ram_percent().unwrap_or(0.0),
            );
        }

        let elapsed = start.elapsed().as_secs_f64();
        tracing::info!("build completed in {:.1}s", elapsed);

        Ok(BuildResult {
            success: true,
            hex_path: link_result.hex_path,
            elf_path: link_result.elf_path,
            size_info: link_result.size_info,
            build_time_secs: elapsed,
            message: format!("Teensy build for {} completed", params.env_name),
        })
    }
}

/// Create a Teensy orchestrator (convenience for get_orchestrator dispatch).
pub fn create() -> Box<dyn BuildOrchestrator> {
    Box::new(TeensyOrchestrator)
}

/// Check if a project is configured for Teensy by reading its platformio.ini.
pub fn is_teensy_project(project_dir: &Path, env_name: &str) -> bool {
    let ini_path = project_dir.join("platformio.ini");
    if let Ok(config) = fbuild_config::PlatformIOConfig::from_path(&ini_path) {
        if let Ok(env) = config.get_env_config(env_name) {
            if let Some(platform) = env.get("platform") {
                return platform == "teensy";
            }
        }
    }
    false
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
