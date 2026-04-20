//! Renesas RA build orchestrator — wires together config, packages, compiler, linker.
//!
//! Build phases:
//! 1. Parse platformio.ini
//! 2. Load board config (uno_r4_wifi, etc.)
//! 3. Ensure ARM GCC toolchain
//! 4. Ensure Renesas cores (ArduinoCore-renesas)
//! 5. Setup build directories
//! 6. Scan source files
//! 7. Compile core sources
//! 8. Compile sketch sources
//! 9. Link (with linker script)
//! 10. Convert to binary + report size

use std::path::{Path, PathBuf};
use std::time::Instant;

use fbuild_core::{Platform, Result};
use serde::Serialize;

use crate::build_fingerprint::{
    hash_watch_set_stamps_cached, save_json, stable_hash_json, FastPathInputs,
    PersistedBuildFingerprint, BUILD_FINGERPRINT_VERSION,
};
use crate::compile_database::CompileDatabase;
use crate::compile_database::TargetArchitecture;
use crate::pipeline;
use crate::zccache::FingerprintWatch;
use crate::{BuildOrchestrator, BuildParams, BuildResult, SourceScanner};

use super::renesas_compiler::RenesasCompiler;
use super::renesas_linker::RenesasLinker;

/// Renesas RA platform build orchestrator.
pub struct RenesasOrchestrator;

#[derive(Debug, Serialize)]
struct RenesasFingerprintMetadata {
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
        build_dir.join("firmware.bin"),
        CompileDatabase::expected_output_path(build_dir, project_dir),
    )
}

impl BuildOrchestrator for RenesasOrchestrator {
    fn platform(&self) -> Platform {
        Platform::RenesasRa
    }

    fn build(&self, params: &BuildParams) -> Result<BuildResult> {
        let start = Instant::now();
        let compiler_cache = crate::zccache::find_zccache().map(std::path::Path::to_path_buf);

        // 1-2. Parse config, load board, setup build dirs, resolve src dir, collect flags
        let mut ctx = pipeline::BuildContext::new(params)?;

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

        // 4. Ensure Renesas cores (ArduinoCore-renesas)
        let framework = fbuild_packages::library::RenesasCores::new(&params.project_dir);
        let framework_dir = fbuild_packages::Package::ensure_installed(&framework)?;
        tracing::info!("Renesas cores at {}", framework_dir.display());

        // 5. Scan sources
        let core_dir = framework.get_core_dir(&ctx.board.core);
        let variant_dir = framework.get_variant_dir(&ctx.board.variant);
        let build_dir = &ctx.build_dir;
        let fingerprint_path = build_fingerprint_path(build_dir);
        let metadata_hash = stable_hash_json(&RenesasFingerprintMetadata {
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
            platform: "renesas-ra".to_string(),
            max_flash: ctx.board.max_flash,
            max_ram: ctx.board.max_ram,
        })?;
        let fingerprint_watches = collect_fast_path_watches(build_dir, &params.project_dir);

        if !params.compiledb_only
            && !params.symbol_analysis
            && params.symbol_analysis_path.is_none()
        {
            let (fast_elf, fast_bin, fast_compile_db) =
                expected_fast_path_artifacts(build_dir, &params.project_dir);
            let required_artifacts = [fast_elf.clone(), fast_bin.clone(), fast_compile_db.clone()];
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
                    "No-op fingerprint matched; reusing existing Renesas artifacts.".to_string(),
                );
                let elapsed = start.elapsed().as_secs_f64();
                return Ok(BuildResult {
                    success: true,
                    firmware_path: Some(fast_bin),
                    elf_path: Some(fast_elf),
                    size_info: hit.size_info,
                    symbol_map: None,
                    build_time_secs: elapsed,
                    message: format!(
                        "Renesas RA ({}) build for {} reused cached artifacts",
                        ctx.board.mcu, params.env_name
                    ),
                    compile_database_path: Some(fast_compile_db),
                    build_log: ctx.build_log,
                });
            }
        }

        let scanner = SourceScanner::new(&ctx.src_dir, &ctx.src_build_dir);
        let sources = scanner.scan_all_filtered(
            Some(&core_dir),
            Some(&variant_dir),
            ctx.source_filter.as_deref(),
        )?;

        tracing::info!(
            "sources: {} sketch, {} core, {} variant",
            sources.sketch_sources.len(),
            sources.core_sources.len(),
            sources.variant_sources.len(),
        );

        // 6. Build include dirs + compiler
        let mcu_config =
            super::mcu_config::get_renesas_config_for_mcu(&ctx.board.mcu.to_lowercase())?;
        let mut defines = ctx.board.get_defines();
        defines.extend(mcu_config.defines_map());
        // Use resolved core_dir/variant_dir instead of get_include_paths() which
        // doesn't account for core_dir overrides.
        let mut include_dirs = vec![core_dir.clone(), variant_dir.clone()];
        // Renesas core has headers in subdirectories (tinyusb/, usb/, cm_backtrace/)
        discover_header_subdirs(&core_dir, &mut include_dirs);
        // Arduino API deprecated compatibility headers
        let api_deprecated = core_dir.join("api").join("deprecated");
        if api_deprecated.is_dir() {
            include_dirs.push(api_deprecated);
        }
        let api_deprecated_avr = core_dir.join("api").join("deprecated-avr-comp");
        if api_deprecated_avr.is_dir() {
            include_dirs.push(api_deprecated_avr);
        }
        // FSP includes from variant's includes.txt (bsp_api.h, CMSIS, etc.)
        include_dirs.extend(framework.get_variant_includes(&ctx.board.variant));
        include_dirs.push(ctx.src_dir.clone());
        pipeline::discover_project_includes(&params.project_dir, &mut include_dirs);
        // Toolchain sysroot includes (ARM CMSIS headers, etc.)
        include_dirs.extend(toolchain.get_include_dirs());

        let compiler = RenesasCompiler::new(
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

        // 7. Create linker (resolve linker script from framework variant)
        let linker_script_path = framework.get_linker_script(&ctx.board.variant);
        let linker = RenesasLinker::new(
            toolchain.get_gcc_path(),
            toolchain.get_ar_path(),
            toolchain.get_objcopy_path(),
            toolchain.get_size_path(),
            linker_script_path,
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
        let c_flags = crate::compiler::Compiler::c_flags(&compiler);
        let cpp_flags = crate::compiler::Compiler::cpp_flags(&compiler);
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
            "Renesas RA",
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

/// Create a Renesas orchestrator (convenience for get_orchestrator dispatch).
pub fn create() -> Box<dyn BuildOrchestrator> {
    Box::new(RenesasOrchestrator)
}

/// Recursively find subdirectories that contain .h files and add them as include dirs.
fn discover_header_subdirs(dir: &Path, include_dirs: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // Check if this directory contains any .h files
            if let Ok(children) = std::fs::read_dir(&path) {
                let has_headers = children
                    .flatten()
                    .any(|e| e.path().extension().is_some_and(|ext| ext == "h"));
                if has_headers {
                    include_dirs.push(path.clone());
                }
            }
            // Recurse into subdirectories
            discover_header_subdirs(&path, include_dirs);
        }
    }
}

/// Check if a project is configured for Renesas by reading its platformio.ini.
pub fn is_renesas_project(project_dir: &Path, env_name: &str) -> bool {
    crate::pipeline::is_platform_project(project_dir, env_name, fbuild_core::Platform::RenesasRa)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_discover_header_subdirs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let core = tmp.path().join("cores/arduino");
        std::fs::create_dir_all(core.join("tinyusb")).unwrap();
        std::fs::create_dir_all(core.join("usb")).unwrap();
        std::fs::create_dir_all(core.join("empty")).unwrap();
        // Write headers
        std::fs::write(core.join("Arduino.h"), "").unwrap();
        std::fs::write(core.join("tinyusb/tusb.h"), "").unwrap();
        std::fs::write(core.join("tinyusb/tusb_option.h"), "").unwrap();
        std::fs::write(core.join("usb/SerialUSB.h"), "").unwrap();
        // usb has only .cpp, no .h? Actually let's add one
        std::fs::write(core.join("usb/usb_bridge.h"), "").unwrap();

        let mut includes = Vec::new();
        discover_header_subdirs(&core, &mut includes);
        // Should find tinyusb/ and usb/ but NOT empty/
        assert!(
            includes.iter().any(|p| p.ends_with("tinyusb")),
            "tinyusb/ should be in includes: {:?}",
            includes
        );
        assert!(
            includes.iter().any(|p| p.ends_with("usb")),
            "usb/ should be in includes: {:?}",
            includes
        );
        assert!(
            !includes.iter().any(|p| p.ends_with("empty")),
            "empty/ should NOT be in includes: {:?}",
            includes
        );
    }

    #[test]
    fn test_renesas_orchestrator_platform() {
        let orch = RenesasOrchestrator;
        assert_eq!(orch.platform(), Platform::RenesasRa);
    }

    #[test]
    fn test_is_renesas_project() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:uno_r4_wifi]\nplatform = renesas-ra\nboard = uno_r4_wifi\nframework = arduino\n",
        )
        .unwrap();
        assert!(is_renesas_project(tmp.path(), "uno_r4_wifi"));
        assert!(!is_renesas_project(tmp.path(), "uno"));
    }

    #[test]
    fn test_is_not_renesas_project() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:uno]\nplatform = atmelavr\nboard = uno\nframework = arduino\n",
        )
        .unwrap();
        assert!(!is_renesas_project(tmp.path(), "uno"));
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
