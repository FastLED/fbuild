//! AVR build orchestrator — wires together config, packages, compiler, linker.
//!
//! Build phases:
//! 1. Parse platformio.ini
//! 2. Load board config
//! 3. Ensure avr-gcc toolchain
//! 4. Ensure Arduino AVR core
//! 5. Setup build directories
//! 6. Scan source files
//! 7. Compile all sources
//! 8. Link into firmware.elf
//! 9. Convert to firmware.hex
//! 10. Report size

use std::path::{Path, PathBuf};
use std::time::Instant;

use fbuild_core::{Platform, Result};
use serde::Serialize;

use crate::build_fingerprint::{
    stable_hash_json, FastPathCheckInputs, FastPathContract, FastPathPersistInputs,
    BUILD_FINGERPRINT_VERSION,
};
use crate::compile_database::{CompileDatabase, TargetArchitecture};
use crate::compiler::Compiler as _;
use crate::pipeline;
use crate::{BuildOrchestrator, BuildParams, BuildResult, SourceScanner};

use super::avr_compiler::AvrCompiler;
use super::avr_linker::AvrLinker;

/// AVR platform build orchestrator.
pub struct AvrOrchestrator;

/// Per-build metadata hashed into the AVR no-op fast path.
///
/// Any field that can change the produced firmware belongs here;
/// a change flips the hash and forces a full rebuild. Keep this in
/// sync with what [`AvrCompiler`] / [`AvrLinker`] actually read off
/// of `BoardConfig` — extra fields only cost a tiny amount of CPU,
/// but missing fields silently let stale artifacts get reused.
#[derive(Debug, Serialize)]
struct AvrFingerprintMetadata {
    version: u32,
    env_name: String,
    profile: String,
    board_name: String,
    board_mcu: String,
    board_define: String,
    board_core: String,
    board_variant: String,
    board_f_cpu: String,
    board_extra_flags: Option<String>,
    board_upload_protocol: Option<String>,
    board_upload_speed: Option<String>,
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

impl BuildOrchestrator for AvrOrchestrator {
    fn platform(&self) -> Platform {
        Platform::AtmelAvr
    }

    fn build(&self, params: &BuildParams) -> Result<BuildResult> {
        let start = Instant::now();
        // Env-gated per-phase timer (FBUILD_PERF_LOG=1); zero-overhead when unset.
        let mut perf = crate::perf_log::PerfTimer::new("avr-orchestrator");

        // 0. Discover zccache compiler cache (startup is deferred until
        // compile work begins). Also used by the fast-path check to short-
        // circuit the watch walk on warm rebuilds.
        let compiler_cache = {
            let _g = perf.phase("zccache-discover");
            crate::zccache::find_zccache().map(std::path::Path::to_path_buf)
        };

        // 1-2. Parse config, load board, setup build dirs, resolve src dir,
        //      collect flags. `new_with_perf` records its own sub-phases
        //      (config-parse, board-load, build-dirs, flag-collect) into
        //      the shared `perf` timer.
        let mut ctx = pipeline::BuildContext::new_with_perf(params, Some(&mut perf))?;

        // 3. Ensure toolchain
        let (toolchain, toolchain_dir) = {
            let _g = perf.phase("toolchain-ensure");
            let toolchain = fbuild_packages::toolchain::AvrToolchain::new(&params.project_dir);
            let toolchain_dir = fbuild_packages::Package::ensure_installed(&toolchain)?;
            (toolchain, toolchain_dir)
        };
        tracing::info!("avr-gcc toolchain at {}", toolchain_dir.display());

        use fbuild_packages::Toolchain as _;
        pipeline::log_toolchain_version(&toolchain.get_gcc_path(), "avr-gcc", &mut ctx.build_log);

        // 4. Ensure Arduino core
        let (_framework_dir, core_dir, variant_dir) = {
            let _g = perf.phase("framework-ensure");
            ensure_avr_framework(
                &params.project_dir,
                &ctx.board.core,
                &ctx.board.variant,
                ctx.board.platform(),
            )?
        };

        // 4.5. Warm-build fast path.
        //
        // All link-affecting config that influences the produced ELF gets
        // folded into `metadata_hash`. If it matches the persisted fingerprint
        // AND the watched inputs (project + resolved libs) are byte-identical
        // since the last successful build, skip the entire compile/link stack.
        // This lives here rather than before `ensure_installed` so the hashed
        // toolchain/framework paths reflect the real install location.
        let build_dir = &ctx.build_dir;
        let metadata_hash = stable_hash_json(&AvrFingerprintMetadata {
            version: BUILD_FINGERPRINT_VERSION,
            env_name: params.env_name.clone(),
            profile: profile_label(params.profile).to_string(),
            board_name: ctx.board.name.clone(),
            board_mcu: ctx.board.mcu.clone(),
            board_define: ctx.board.board.clone(),
            board_core: ctx.board.core.clone(),
            board_variant: ctx.board.variant.clone(),
            board_f_cpu: ctx.board.f_cpu.clone(),
            board_extra_flags: ctx.board.extra_flags.clone(),
            board_upload_protocol: ctx.board.upload_protocol.clone(),
            board_upload_speed: ctx.board.upload_speed.clone(),
            platform: "atmelavr".to_string(),
            max_flash: ctx.board.max_flash,
            max_ram: ctx.board.max_ram,
        })?;
        let (fast_elf, fast_hex, fast_compile_db) =
            expected_fast_path_artifacts(build_dir, &params.project_dir);
        let fast_path = {
            let _g = perf.phase("fp-watches-collect");
            FastPathContract::for_project_outputs(
                build_dir,
                &params.project_dir,
                [fast_elf.clone(), fast_hex.clone(), fast_compile_db.clone()],
            )
        };

        if !params.compiledb_only
            && !params.symbol_analysis
            && params.symbol_analysis_path.is_none()
        {
            let _fast_path_phase = perf.phase("fast-path-check");
            let inputs = FastPathCheckInputs {
                metadata_hash: &metadata_hash,
                extra_artifact_ok: None,
                watch_set_cache: params.watch_set_cache.as_deref(),
                compiler_cache: compiler_cache.as_deref(),
            };
            if let Some(hit) = crate::build_fingerprint::fast_path_check(&fast_path, &inputs)? {
                ctx.build_log
                    .push("No-op fingerprint matched; reusing existing AVR artifacts.".to_string());
                let elapsed = start.elapsed().as_secs_f64();
                return Ok(BuildResult {
                    success: true,
                    firmware_path: Some(fast_hex),
                    elf_path: Some(fast_elf),
                    size_info: hit.size_info,
                    symbol_map: None,
                    build_time_secs: elapsed,
                    message: format!(
                        "AVR ({}) build for {} reused cached artifacts",
                        ctx.board.mcu, params.env_name
                    ),
                    compile_database_path: Some(fast_compile_db),
                    build_log: ctx.build_log,
                });
            }
        }

        // 5. Scan sources
        let sources = {
            let _g = perf.phase("source-scan");
            let scanner = SourceScanner::new(&ctx.src_dir, &ctx.src_build_dir);
            scanner.scan_all_filtered(
                Some(&core_dir),
                Some(&variant_dir),
                ctx.source_filter.as_deref(),
            )?
        };

        tracing::info!(
            "sources: {} sketch, {} core, {} variant",
            sources.sketch_sources.len(),
            sources.core_sources.len(),
            sources.variant_sources.len(),
        );

        // 6. Build include dirs + compiler
        let defines = ctx.board.get_defines();
        // Use the resolved core_dir/variant_dir directly — board.get_include_paths()
        // uses the raw board core name which may differ from the actual directory
        // (e.g. MiniCore's core dir is "MCUdude_corefiles", not "MiniCore").
        let mut include_dirs = vec![core_dir.clone(), variant_dir.clone()];
        include_dirs.push(ctx.src_dir.clone());
        pipeline::discover_project_includes(&params.project_dir, &mut include_dirs);
        // Toolchain sysroot includes (avr/io.h, etc.)
        include_dirs.extend(toolchain.get_include_dirs());

        let mcu_config = super::mcu_config::get_avr_config()?;

        let compiler = AvrCompiler::new(
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

        // 7. Create linker
        let linker = AvrLinker::new(
            toolchain.get_gcc_path(),
            toolchain.get_ar_path(),
            toolchain.get_objcopy_path(),
            toolchain.get_size_path(),
            &ctx.board.mcu,
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
            TargetArchitecture::Avr,
            "AVR",
            start,
        )?;

        // 10. Persist fingerprint so the next warm invocation can hit the
        // fast path. Skip this for compile-db-only / symbol-analysis runs
        // — they don't produce the full artifact set the fast path
        // requires.
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

/// Create an AVR orchestrator (convenience for get_orchestrator dispatch).
pub fn create() -> Box<dyn BuildOrchestrator> {
    Box::new(AvrOrchestrator)
}

/// Select and install the correct AVR Arduino framework based on the board's core name.
///
/// Uses the data-driven `avr_frameworks.json` registry to resolve the correct
/// framework package (GitHub URL, version) for any board core.
/// For `AtmelMegaAvr` boards whose core is `"arduino"`, the lookup key is remapped
/// to `"arduino_megaavr"` so they get `ArduinoCore-megaavr` (which contains the
/// megaAVR variants like `nona4809`) instead of `ArduinoCore-avr`.
/// Returns (framework_root, core_dir, variant_dir).
fn ensure_avr_framework(
    project_dir: &Path,
    core_name: &str,
    variant_name: &str,
    platform: Option<fbuild_core::Platform>,
) -> fbuild_core::Result<(PathBuf, PathBuf, PathBuf)> {
    use fbuild_packages::Package;

    // megaAVR boards (e.g. nano_every) share core name "arduino" with standard AVR
    // but need ArduinoCore-megaavr instead of ArduinoCore-avr.
    let lookup_key =
        if platform == Some(fbuild_core::Platform::AtmelMegaAvr) && core_name == "arduino" {
            "arduino_megaavr"
        } else {
            core_name
        };

    let framework = fbuild_packages::library::AvrFramework::for_core(lookup_key, project_dir)?;
    let framework_dir = framework.ensure_installed()?;
    tracing::info!(
        "AVR framework for core '{}' (lookup '{}') at {}",
        core_name,
        lookup_key,
        framework_dir.display()
    );
    let core_dir = framework.get_core_dir(core_name);
    let variant_dir = framework.get_variant_dir(variant_name);
    Ok((framework_dir, core_dir, variant_dir))
}

/// Check if a project is configured for AVR by reading its platformio.ini.
pub fn is_avr_project(project_dir: &Path, env_name: &str) -> bool {
    crate::pipeline::is_platform_project(project_dir, env_name, fbuild_core::Platform::AtmelAvr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_avr_orchestrator_platform() {
        let orch = AvrOrchestrator;
        assert_eq!(orch.platform(), Platform::AtmelAvr);
    }

    #[test]
    fn test_is_avr_project() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:uno]\nplatform = atmelavr\nboard = uno\nframework = arduino\n",
        )
        .unwrap();
        assert!(is_avr_project(tmp.path(), "uno"));
        assert!(!is_avr_project(tmp.path(), "esp32"));
    }

    #[test]
    fn test_is_not_avr_project() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:esp32]\nplatform = espressif32\nboard = esp32dev\nframework = arduino\n",
        )
        .unwrap();
        assert!(!is_avr_project(tmp.path(), "esp32"));
    }

    /// Verify that megaAVR boards remap "arduino" core to "arduino_megaavr" framework.
    #[test]
    fn test_megaavr_core_remaps_to_megaavr_framework() {
        let core = "arduino";
        let platform = Some(Platform::AtmelMegaAvr);
        let lookup_key = if platform == Some(Platform::AtmelMegaAvr) && core == "arduino" {
            "arduino_megaavr"
        } else {
            core
        };
        assert_eq!(lookup_key, "arduino_megaavr");

        // Standard AVR should NOT remap
        let platform_avr = Some(Platform::AtmelAvr);
        let lookup_avr = if platform_avr == Some(Platform::AtmelMegaAvr) && core == "arduino" {
            "arduino_megaavr"
        } else {
            core
        };
        assert_eq!(lookup_avr, "arduino");
    }
}
