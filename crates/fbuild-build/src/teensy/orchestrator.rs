//! Teensy build orchestrator â€” wires together config, packages, compiler, linker.
//!
//! Build phases:
//! 1. Parse platformio.ini
//! 2. Load board config (teensy40/teensy41)
//! 3. Ensure Teensy-compatible ARM GCC toolchain
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
use serde::Serialize;

use crate::build_fingerprint::{
    expected_fast_path_artifacts, stable_hash_json, FastPathCheckInputs, FastPathContract,
    FastPathPersistInputs, BUILD_FINGERPRINT_VERSION,
};
use crate::compile_database::TargetArchitecture;
use crate::compiler::Compiler as _;
use crate::framework_libs::{
    library_select_kv_store, resolve_framework_library_sources,
    resolve_framework_library_sources_cached,
};
use crate::pipeline;
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
    eh_frame_policy: &'static str,
}

fn profile_label(profile: fbuild_core::BuildProfile) -> &'static str {
    match profile {
        fbuild_core::BuildProfile::Release => "release",
        fbuild_core::BuildProfile::Quick => "quick",
    }
}

#[async_trait::async_trait]
impl BuildOrchestrator for TeensyOrchestrator {
    fn platform(&self) -> Platform {
        Platform::Teensy
    }

    async fn build(&self, params: &BuildParams) -> Result<BuildResult> {
        let start = Instant::now();
        let compiler_cache: Option<std::path::PathBuf> = None;

        // 1-2. Parse config, load board, setup build dirs, resolve src dir, collect flags
        let mut ctx = pipeline::BuildContext::new(params).await?;

        // Compute eh_frame strip policy once per build (FastLED/fbuild#244).
        // No sdkconfig on Teensy.
        let eh_frame_policy =
            crate::eh_frame_policy_compute::compute_eh_frame_policy(&ctx, params.profile, None);

        // Need board_id for linker script lookup later
        let env_config = ctx.config.get_env_config(&params.env_name)?;
        let board_id = env_config.get("board").ok_or_else(|| {
            fbuild_core::FbuildError::ConfigError("missing 'board' in environment config".into())
        })?;

        // 3. Ensure Teensy-compatible ARM GCC toolchain
        let toolchain = fbuild_packages::toolchain::TeensyArmToolchain::new(&params.project_dir);
        let toolchain_dir = fbuild_packages::Package::ensure_installed(&toolchain).await?;
        tracing::info!("Teensy ARM GCC toolchain at {}", toolchain_dir.display());

        use fbuild_packages::Toolchain;
        pipeline::log_toolchain_version(
            &toolchain.get_gcc_path(),
            "arm-none-eabi-gcc",
            &mut ctx.build_log,
        )
        .await;

        // 4. Ensure Teensy cores
        let framework = fbuild_packages::library::TeensyCores::new(&params.project_dir);
        let framework_dir = fbuild_packages::Package::ensure_installed(&framework).await?;
        tracing::info!("Teensy cores at {}", framework_dir.display());

        let core_dir = framework.get_core_dir(&ctx.board.core);
        let build_dir = &ctx.build_dir;
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
            eh_frame_policy: match eh_frame_policy {
                crate::eh_frame_policy::EhFramePolicy::Strip => "strip",
                crate::eh_frame_policy::EhFramePolicy::Preserve => "preserve",
            },
        })?;
        let (fast_elf, [fast_hex], fast_compile_db) =
            expected_fast_path_artifacts(build_dir, &params.project_dir, ["firmware.hex"]);
        let fast_path = FastPathContract::for_project_outputs(
            build_dir,
            &params.project_dir,
            [fast_elf.clone(), fast_hex.clone(), fast_compile_db.clone()],
        );

        if !params.compiledb_only
            && !params.symbol_analysis
            && params.symbol_analysis_path.is_none()
        {
            let inputs = FastPathCheckInputs {
                metadata_hash: &metadata_hash,
                extra_artifact_ok: None,
                watch_set_cache: params.watch_set_cache.as_deref(),
                compiler_cache: compiler_cache.as_deref(),
            };
            if let Some(hit) = crate::build_fingerprint::fast_path_check(&fast_path, &inputs)? {
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

        let framework_libs = framework.get_framework_libraries();
        // WHY: Teensy 3.x/4.x and TeensyLC all share teensyduino's
        // arm-none-eabi toolchain â€” a single stable triple covers every
        // board this orchestrator handles. The triple feeds the cache key
        // so bumping it invalidates the entire teensy slice without
        // touching SCANNER_VERSION / LDF_MODE_VERSION.
        let framework_info = fbuild_packages::Package::get_info(&framework);
        let framework_library_sources = match library_select_kv_store() {
            Some(store) => {
                let key_inputs = fbuild_library_select::cache::CacheKeyInputs {
                    toolchain_triple: "teensy-arm-none-eabi",
                    framework_install_path: &framework_info.install_path,
                    framework_version: &framework_info.version,
                };
                resolve_framework_library_sources_cached(
                    &framework_libs,
                    &params.project_dir,
                    &ctx.src_dir,
                    &key_inputs,
                    store,
                )
            }
            None => resolve_framework_library_sources(
                &framework_libs,
                &params.project_dir,
                &ctx.src_dir,
            ),
        };
        if !framework_library_sources.is_empty() {
            tracing::info!(
                "Teensy framework library sources added: {}",
                framework_library_sources.len()
            );
            sources.core_sources.extend(framework_library_sources);
        }

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
        include_dirs.extend(framework.get_framework_library_include_dirs());
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
        .with_build_unflags(ctx.build_unflags.clone())
        .with_eh_frame_policy(eh_frame_policy);

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
            ctx.board.cmsis_dsp_lib.clone(),
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
        ).await?;

        if build_result.success
            && !params.compiledb_only
            && !params.symbol_analysis
            && params.symbol_analysis_path.is_none()
        {
            crate::build_fingerprint::persist_fast_path_success(
                &fast_path,
                &FastPathPersistInputs {
                    metadata_hash: &metadata_hash,
                    size_info: build_result.size_info.clone(),
                    watch_set_cache: params.watch_set_cache.as_deref(),
                    compiler_cache: compiler_cache.as_deref(),
                },
            );
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
    fn test_fast_path_contract_preserves_missing_dep_libs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let build_dir = tmp.path().join("build");
        let project_dir = tmp.path().join("project");
        std::fs::create_dir_all(&build_dir).unwrap();
        std::fs::create_dir_all(&project_dir).unwrap();

        let contract = FastPathContract::for_project_outputs(
            &build_dir,
            &project_dir,
            Vec::<std::path::PathBuf>::new(),
        );
        assert_eq!(contract.watches().len(), 2);
        assert_eq!(contract.watches()[0].root, project_dir);
        assert_eq!(contract.watches()[1].root, build_dir.join("libs"));
    }
}
