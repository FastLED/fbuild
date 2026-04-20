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

use std::path::{Path, PathBuf};
use std::time::Instant;

use fbuild_core::{Platform, Result};
use serde::Serialize;

use crate::build_fingerprint::{
    hash_watch_set_stamps_cached, save_json, stable_hash_json, FastPathInputs,
    PersistedBuildFingerprint, BUILD_FINGERPRINT_VERSION,
};
use crate::compile_database::{CompileDatabase, TargetArchitecture};
use crate::compiler::Compiler as _;
use crate::pipeline;
use crate::zccache::FingerprintWatch;
use crate::{BuildOrchestrator, BuildParams, BuildResult, SourceScanner};

use super::teensy_compiler::TeensyCompiler;
use super::teensy_linker::TeensyLinker;

/// Teensy platform build orchestrator.
pub struct TeensyOrchestrator;

#[derive(Debug, Serialize)]
struct TeensyFingerprintMetadata {
    version: u32,
    env_name: String,
    profile: String,
    board_name: String,
    board_mcu: String,
    board_define: String,
    board_core: String,
    board_f_cpu: String,
    board_extra_flags: Option<String>,
    board_ldscript: Option<String>,
    platform: String,
    max_flash: Option<u64>,
    max_ram: Option<u64>,
}

fn profile_label(profile: fbuild_core::BuildProfile) -> &'static str {
    match profile {
        fbuild_core::BuildProfile::Release => "release",
        fbuild_core::BuildProfile::Quick => "quick",
    }
}

fn build_fingerprint_path(build_dir: &Path) -> PathBuf {
    build_dir.join("build_fingerprint.json")
}

fn collect_fast_path_watches(build_dir: &Path, project_dir: &Path) -> Vec<FingerprintWatch> {
    let mut watches = Vec::new();
    if let Some(watch) =
        crate::build_fingerprint::fast_path_watch("project", build_dir, project_dir)
    {
        watches.push(watch);
    }
    let resolved_libs_dir = build_dir.join("libs");
    if let Some(watch) =
        crate::build_fingerprint::fast_path_watch("dep_libs", build_dir, &resolved_libs_dir)
    {
        watches.push(watch);
    }
    watches
}

fn expected_fast_path_artifacts(
    build_dir: &Path,
    project_dir: &Path,
) -> (PathBuf, PathBuf, PathBuf) {
    (
        build_dir.join("firmware.elf"),
        build_dir.join("firmware.hex"),
        CompileDatabase::expected_output_path(build_dir, project_dir),
    )
}

impl BuildOrchestrator for TeensyOrchestrator {
    fn platform(&self) -> Platform {
        Platform::Teensy
    }

    fn build(&self, params: &BuildParams) -> Result<BuildResult> {
        let start = Instant::now();
        let compiler_cache = crate::zccache::find_zccache().map(std::path::Path::to_path_buf);

        // 1-2. Parse config, load board, setup build dirs, resolve src dir, collect flags
        let mut ctx = pipeline::BuildContext::new(params)?;

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

        let core_dir = framework.get_core_dir(&ctx.board.core);
        let build_dir = &ctx.build_dir;
        let fingerprint_path = build_fingerprint_path(build_dir);
        let metadata_hash = stable_hash_json(&TeensyFingerprintMetadata {
            version: BUILD_FINGERPRINT_VERSION,
            env_name: params.env_name.clone(),
            profile: profile_label(params.profile).to_string(),
            board_name: ctx.board.name.clone(),
            board_mcu: ctx.board.mcu.clone(),
            board_define: ctx.board.board.clone(),
            board_core: ctx.board.core.clone(),
            board_f_cpu: ctx.board.f_cpu.clone(),
            board_extra_flags: ctx.board.extra_flags.clone(),
            board_ldscript: ctx.board.ldscript.clone(),
            platform: "teensy".to_string(),
            max_flash: ctx.board.max_flash,
            max_ram: ctx.board.max_ram,
        })?;
        let fingerprint_watches = collect_fast_path_watches(build_dir, &params.project_dir);

        if !params.compiledb_only
            && !params.symbol_analysis
            && params.symbol_analysis_path.is_none()
        {
            let (fast_elf, fast_hex, fast_compile_db) =
                expected_fast_path_artifacts(build_dir, &params.project_dir);
            let required_artifacts = [fast_elf.clone(), fast_hex.clone(), fast_compile_db.clone()];
            let inputs = FastPathInputs {
                fingerprint_path: &fingerprint_path,
                metadata_hash: &metadata_hash,
                watches: &fingerprint_watches,
                required_artifacts: &required_artifacts,
                extra_artifact_ok: None,
                watch_set_cache: params.watch_set_cache.as_deref(),
                compiler_cache: compiler_cache.as_deref(),
            };
            if let Some(hit) = crate::build_fingerprint::fast_path_check(&inputs)? {
                ctx.build_log.push(
                    "No-op fingerprint matched; reusing existing Teensy artifacts.".to_string(),
                );
                let elapsed = start.elapsed().as_secs_f64();
                return Ok(BuildResult {
                    success: true,
                    firmware_path: Some(fast_hex),
                    elf_path: Some(fast_elf),
                    size_info: hit.size_info,
                    symbol_map: None,
                    build_time_secs: elapsed,
                    message: format!(
                        "Teensy ({}) build for {} reused cached artifacts",
                        ctx.board.mcu, params.env_name
                    ),
                    compile_database_path: Some(fast_compile_db),
                    build_log: ctx.build_log,
                });
            }
        }

        // 5. Scan sources (Teensy: no variants, exclude Blink.cc test sketch)
        let scanner = SourceScanner::new(&ctx.src_dir, &ctx.src_build_dir);
        let mut sources =
            scanner.scan_all_filtered(Some(&core_dir), None, ctx.source_filter.as_deref())?;
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
            include_dirs.clone(),
            mcu_config.clone(),
            params.profile,
            params.verbose,
        )
        .with_build_unflags(ctx.build_unflags.clone());

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

        // 8. Build LibraryBuildEnv for project-as-library compilation
        let gcc_path = toolchain.get_gcc_path();
        let gxx_path = toolchain.get_gxx_path();
        let ar_path = toolchain.get_ar_path();
        let gcc_ar_path = toolchain.get_gcc_ar_path();
        let c_flags = compiler.c_flags();
        let cpp_flags = compiler.cpp_flags();
        // Use gcc-ar for LTO archives so the linker-plugin index is written.
        let lib_ar_path = pipeline::pick_archiver(&ar_path, &gcc_ar_path, &c_flags, &cpp_flags);
        let lib_env = pipeline::LibraryBuildEnv {
            gcc_path: &gcc_path,
            gxx_path: &gxx_path,
            ar_path: lib_ar_path,
            c_flags: &c_flags,
            cpp_flags: &cpp_flags,
            include_dirs: &include_dirs,
            verbose: params.verbose,
            jobs: crate::parallel::effective_jobs(params.jobs),
            compiler_cache: None,
        };

        // 9. Run shared sequential build pipeline
        let build_result = pipeline::run_sequential_build_with_libs(
            &compiler,
            &linker,
            ctx,
            params,
            &sources,
            &[],
            Some(&lib_env),
            TargetArchitecture::Arm,
            "Teensy",
            start,
        )?;

        if build_result.success
            && !params.compiledb_only
            && !params.symbol_analysis
            && params.symbol_analysis_path.is_none()
        {
            let persisted_fingerprint = PersistedBuildFingerprint {
                version: BUILD_FINGERPRINT_VERSION,
                metadata_hash: metadata_hash.clone(),
                file_set_hash: match hash_watch_set_stamps_cached(
                    &fingerprint_watches,
                    params.watch_set_cache.as_deref(),
                ) {
                    Ok(hash) => Some(hash),
                    Err(e) => {
                        tracing::warn!("failed to hash watched inputs for fingerprint save: {}", e);
                        None
                    }
                },
                size_info: build_result.size_info.clone(),
            };
            if let Err(e) = save_json(&fingerprint_path, &persisted_fingerprint) {
                tracing::warn!("failed to write build fingerprint: {}", e);
            }
            if let Some(ref zcc) = compiler_cache {
                for watch in &fingerprint_watches {
                    if let Err(e) = crate::zccache::mark_fingerprint_success(zcc, watch) {
                        tracing::warn!(
                            "failed to mark zccache fingerprint success for {}: {}",
                            watch.root.display(),
                            e
                        );
                    }
                }
            }
        }

        Ok(build_result)
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

    #[test]
    fn test_collect_fast_path_watches_skips_missing_dep_libs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let build_dir = tmp.path().join("build");
        let project_dir = tmp.path().join("project");
        std::fs::create_dir_all(&build_dir).unwrap();
        std::fs::create_dir_all(&project_dir).unwrap();

        let watches = collect_fast_path_watches(&build_dir, &project_dir);
        assert_eq!(watches.len(), 1);
        assert_eq!(watches[0].root, project_dir);
    }

    #[test]
    fn test_expected_fast_path_artifacts_follow_compile_db_location() {
        let tmp = tempfile::TempDir::new().unwrap();
        let build_dir = tmp.path().join("build");
        let app_project = tmp.path().join("app");
        let lib_project = tmp.path().join("libproj");
        std::fs::create_dir_all(&build_dir).unwrap();
        std::fs::create_dir_all(&app_project).unwrap();
        std::fs::create_dir_all(&lib_project).unwrap();
        std::fs::write(lib_project.join("library.json"), r#"{"name":"libproj"}"#).unwrap();

        let (_, _, app_compile_db) = expected_fast_path_artifacts(&build_dir, &app_project);
        let (_, _, lib_compile_db) = expected_fast_path_artifacts(&build_dir, &lib_project);

        assert_eq!(app_compile_db, app_project.join("compile_commands.json"));
        assert_eq!(lib_compile_db, build_dir.join("compile_commands.json"));
    }
}
