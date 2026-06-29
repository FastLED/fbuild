//! SAM/SAMD build orchestrator â€” wires together config, packages, compiler, linker.
//!
//! Handles both SAM (Due/SAM3X) and SAMD (SAMD21/SAMD51) boards under the
//! `atmelsam` platform. Selects the correct Arduino core:
//! - SAM3X â†’ ArduinoCore-sam
//! - SAMD21/51 â†’ ArduinoCore-samd (Adafruit fork)
//!
//! Build phases:
//! 1. Parse platformio.ini
//! 2. Load board config
//! 3. Ensure ARM GCC toolchain
//! 4. Ensure correct Arduino core
//! 5. Setup build directories
//! 6. Scan source files
//! 7. Compile core + sketch sources
//! 8. Link (with linker script)
//! 9. Convert to binary + report size

use std::path::{Path, PathBuf};
use std::time::Instant;

use fbuild_core::{Platform, Result};
use serde::Serialize;

use crate::build_fingerprint::{
    expected_fast_path_artifacts, stable_hash_json, FastPathCheckInputs, FastPathContract,
    FastPathPersistInputs, BUILD_FINGERPRINT_VERSION,
};
use crate::compile_database::TargetArchitecture;
use crate::pipeline;
use crate::{BuildOrchestrator, BuildParams, BuildResult, SourceScanner};

use super::sam_compiler::SamCompiler;
use super::sam_linker::SamLinker;

/// Returns true if the MCU is a SAMD (not classic SAM3X).
fn is_samd_mcu(mcu: &str) -> bool {
    let m = mcu.to_lowercase();
    m.starts_with("samd21") || m.starts_with("samd51")
}

/// SAM platform build orchestrator.
pub struct SamOrchestrator;

#[derive(Debug, Serialize)]
struct SamFingerprintMetadata {
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
    board_vid: Option<String>,
    board_pid: Option<String>,
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

#[async_trait::async_trait]
impl BuildOrchestrator for SamOrchestrator {
    fn platform(&self) -> Platform {
        Platform::AtmelSam
    }

    async fn build(&self, params: &BuildParams) -> Result<BuildResult> {
        let start = Instant::now();
        let compiler_cache: Option<std::path::PathBuf> = None;

        // 1-2. Parse config, load board, setup build dirs, resolve src dir, collect flags
        let mut ctx = pipeline::BuildContext::new(params).await?;

        // 3. Ensure ARM GCC toolchain
        let toolchain = fbuild_packages::toolchain::ArmToolchain::new(&params.project_dir);
        let toolchain_dir = fbuild_packages::Package::ensure_installed(&toolchain).await?;
        tracing::info!("arm-none-eabi toolchain at {}", toolchain_dir.display());

        use fbuild_packages::Toolchain;
        pipeline::log_toolchain_version(
            &toolchain.get_gcc_path(),
            "arm-none-eabi-gcc",
            &mut ctx.build_log,
        )
        .await;

        // 4. Ensure correct Arduino core based on MCU family
        let (framework_dir, core_dir, variant_dir, linker_script_path, system_includes) =
            if is_samd_mcu(&ctx.board.mcu) {
                install_samd_core(params, &ctx.board.core, &ctx.board.variant).await?
            } else {
                install_sam_core(params, &ctx.board.core, &ctx.board.variant).await?
            };

        let build_dir = &ctx.build_dir;
        let metadata_hash = stable_hash_json(&SamFingerprintMetadata {
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
            board_vid: ctx.board.vid.clone(),
            board_pid: ctx.board.pid.clone(),
            platform: "atmelsam".to_string(),
            max_flash: ctx.board.max_flash,
            max_ram: ctx.board.max_ram,
        })?;
        let (fast_elf, [fast_bin], fast_compile_db) =
            expected_fast_path_artifacts(build_dir, &params.project_dir, ["firmware.bin"]);
        let fast_path = FastPathContract::for_project_outputs(
            build_dir,
            &params.project_dir,
            [fast_elf.clone(), fast_bin.clone(), fast_compile_db.clone()],
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
                ctx.build_log
                    .push("No-op fingerprint matched; reusing existing SAM artifacts.".to_string());
                let elapsed = start.elapsed().as_secs_f64();
                return Ok(BuildResult {
                    success: true,
                    firmware_path: Some(fast_bin),
                    elf_path: Some(fast_elf),
                    size_info: hit.size_info,
                    symbol_map: None,
                    build_time_secs: elapsed,
                    message: format!(
                        "SAM ({}) build for {} reused cached artifacts",
                        ctx.board.mcu, params.env_name
                    ),
                    compile_database_path: Some(fast_compile_db),
                    build_log: ctx.build_log,
                });
            }
        }

        // 5. Scan sources
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
        let mcu_lower = ctx.board.mcu.to_lowercase();
        let mcu_config = super::mcu_config::get_sam_config_for_mcu(&mcu_lower)?;
        let mut defines = ctx.board.get_defines();
        defines.extend(mcu_config.defines_map());

        // Add MCU-specific device defines (required by CMSIS headers)
        // Strip AT91 prefix: board JSON has "AT91SAM3X8E" but CMSIS expects __SAM3X8E__
        let mcu_upper = ctx.board.mcu.to_uppercase();
        let mcu_define = mcu_upper.strip_prefix("AT91").unwrap_or(&mcu_upper);
        defines.insert(format!("__{}__", mcu_define), "1".to_string());
        // USB support (PluggableUSB, CDC)
        defines.insert("USBCON".to_string(), "1".to_string());
        if let Some(ref vid) = ctx.board.vid {
            defines.insert("USB_VID".to_string(), vid.clone());
        }
        if let Some(ref pid) = ctx.board.pid {
            defines.insert("USB_PID".to_string(), pid.clone());
        }
        if is_samd_mcu(&ctx.board.mcu) {
            if mcu_upper.starts_with("SAMD51") {
                defines.insert("__SAMD51__".to_string(), "1".to_string());
                defines.insert("__FPU_PRESENT".to_string(), "1".to_string());
                defines.insert("ARM_MATH_CM4".to_string(), "1".to_string());
            } else {
                defines.insert("ARM_MATH_CM0PLUS".to_string(), "1".to_string());
            }
        }

        let mut include_dirs = ctx.board.get_include_paths(&framework_dir);
        include_dirs.push(ctx.src_dir.clone());
        pipeline::discover_project_includes(&params.project_dir, &mut include_dirs);
        // Toolchain sysroot includes
        include_dirs.extend(toolchain.get_include_dirs());
        // Platform system includes (CMSIS, libsam, etc.)
        include_dirs.extend(system_includes);

        let compiler = SamCompiler::new(
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
        let mut linker = SamLinker::new(
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
        // Add variant directory as linker search path for system libraries
        // (e.g. libsam_sam3x8e_gcc_rel.a for Due)
        linker.add_lib_dirs(vec![variant_dir]);
        if !is_samd_mcu(&ctx.board.mcu) {
            // Due needs the pre-built libsam system library
            linker.add_libs(vec!["sam_sam3x8e_gcc_rel".to_string()]);
        }

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
            "SAM",
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

/// Install ArduinoCore-sam for classic SAM3X boards (Due).
///
/// Returns (framework_dir, core_dir, variant_dir, linker_script, system_includes).
async fn install_sam_core(
    params: &BuildParams,
    core_name: &str,
    variant_name: &str,
) -> Result<(PathBuf, PathBuf, PathBuf, PathBuf, Vec<PathBuf>)> {
    let framework = fbuild_packages::library::SamCores::new(&params.project_dir);
    let framework_dir = fbuild_packages::Package::ensure_installed(&framework).await?;
    tracing::info!("SAM cores at {}", framework_dir.display());

    let core_dir = framework.get_core_dir(core_name);
    let variant_dir = framework.get_variant_dir(variant_name);
    let linker_script = framework.get_linker_script(variant_name);

    // SAM system includes: libsam (chip.h, include/), CMSIS headers, ATMEL device headers
    let system_dir = framework.get_system_dir();
    let mut includes = Vec::new();
    if system_dir.exists() {
        includes.push(system_dir.join("libsam"));
        includes.push(system_dir.join("libsam").join("include"));
        includes.push(system_dir.join("CMSIS").join("CMSIS").join("Include"));
        includes.push(system_dir.join("CMSIS").join("Device").join("ATMEL"));
    }

    Ok((
        framework_dir,
        core_dir,
        variant_dir,
        linker_script,
        includes,
    ))
}

/// Install Adafruit ArduinoCore-samd for SAMD21/SAMD51 boards.
///
/// Returns (framework_dir, core_dir, variant_dir, linker_script, system_includes).
async fn install_samd_core(
    params: &BuildParams,
    core_name: &str,
    variant_name: &str,
) -> Result<(PathBuf, PathBuf, PathBuf, PathBuf, Vec<PathBuf>)> {
    let framework = fbuild_packages::library::SamdCores::new(&params.project_dir);
    let framework_dir = fbuild_packages::Package::ensure_installed(&framework).await?;
    tracing::info!("SAMD cores at {}", framework_dir.display());

    let core_dir = framework.get_core_dir(core_name);
    let variant_dir = framework.get_variant_dir(variant_name);
    let linker_script = framework.get_linker_script(variant_name);

    // SAMD core needs external CMSIS and CMSIS-Atmel packages for device headers
    let cmsis = fbuild_packages::library::CmsisFramework::new(&params.project_dir);
    let cmsis_dir = fbuild_packages::Package::ensure_installed(&cmsis).await?;
    tracing::info!("CMSIS at {}", cmsis_dir.display());

    let cmsis_atmel = fbuild_packages::library::CmsisAtmel::new(&params.project_dir);
    let _cmsis_atmel_dir = fbuild_packages::Package::ensure_installed(&cmsis_atmel).await?;
    tracing::info!("CMSIS-Atmel installed");

    let mut includes = vec![
        cmsis.get_core_include_dir(),
        cmsis.get_dsp_include_dir(),
        cmsis_atmel.get_device_include_dir(),
    ];
    // Also include the variant dir for variant.h
    includes.push(variant_dir.clone());
    // And the resolved core dir for Arduino.h / WVariant.h.
    //
    // `BoardConfig::get_include_paths` already emits
    // `framework_root/cores/<board.core>`, which for Adafruit SAMD boards is
    // a vendor-brand label (e.g. "adafruit") that the framework doesn't
    // actually ship as a directory â€” only `cores/arduino/` exists. That
    // literal path becomes a phantom `-I` and `#include "Arduino.h"` /
    // `#include "WVariant.h"` lookups miss. `core_dir` here was produced by
    // `SamdCores::get_core_dir` which falls back to `cores/arduino/` when the
    // literal subdir is absent (FastLED/fbuild#319), so injecting it into the
    // compile include path is what actually resolves the headers. Putting
    // it BEFORE the phantom is a no-op but makes the active dir the first
    // hit during search, matching what PlatformIO's atmelsam builder does.
    includes.insert(0, core_dir.clone());

    Ok((
        framework_dir,
        core_dir,
        variant_dir,
        linker_script,
        includes,
    ))
}

/// Create a SAM orchestrator (convenience for get_orchestrator dispatch).
pub fn create() -> Box<dyn BuildOrchestrator> {
    Box::new(SamOrchestrator)
}

/// Check if a project is configured for SAM by reading its platformio.ini.
pub fn is_sam_project(project_dir: &Path, env_name: &str) -> bool {
    crate::pipeline::is_platform_project(project_dir, env_name, fbuild_core::Platform::AtmelSam)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sam_orchestrator_platform() {
        let orch = SamOrchestrator;
        assert_eq!(orch.platform(), Platform::AtmelSam);
    }

    #[test]
    fn test_fast_path_contract_preserves_missing_dep_libs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let build_dir = tmp.path().join("build");
        let project_dir = tmp.path().join("project");
        std::fs::create_dir_all(&build_dir).unwrap();
        std::fs::create_dir_all(&project_dir).unwrap();

        let contract =
            FastPathContract::for_project_outputs(&build_dir, &project_dir, Vec::<PathBuf>::new());
        assert_eq!(contract.watches().len(), 2);
        assert_eq!(contract.watches()[0].root, project_dir);
        assert_eq!(contract.watches()[1].root, build_dir.join("libs"));
    }
}
