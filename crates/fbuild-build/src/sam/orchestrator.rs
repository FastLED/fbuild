//! SAM/SAMD build orchestrator — wires together config, packages, compiler, linker.
//!
//! Handles both SAM (Due/SAM3X) and SAMD (SAMD21/SAMD51) boards under the
//! `atmelsam` platform. Selects the correct Arduino core:
//! - SAM3X → ArduinoCore-sam
//! - SAMD21/51 → ArduinoCore-samd (Adafruit fork)
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

impl BuildOrchestrator for SamOrchestrator {
    fn platform(&self) -> Platform {
        Platform::AtmelSam
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

        // 3. Ensure ARM GCC toolchain
        let toolchain = fbuild_packages::toolchain::ArmToolchain::new(&params.project_dir);
        let toolchain_dir = fbuild_packages::Package::ensure_installed(&toolchain)?;
        tracing::info!("arm-none-eabi toolchain at {}", toolchain_dir.display());

        use fbuild_packages::Toolchain;
        pipeline::log_toolchain_version(
            &toolchain.get_gcc_path(),
            "arm-none-eabi-gcc",
            &mut ctx.build_log,
        );

        // 4. Ensure correct Arduino core based on MCU family
        let (framework_dir, core_dir, variant_dir, linker_script_path, system_includes) =
            if is_samd_mcu(&ctx.board.mcu) {
                install_samd_core(params, &ctx.board.core, &ctx.board.variant)?
            } else {
                install_sam_core(params, &ctx.board.core, &ctx.board.variant)?
            };

        // 5. Scan sources
        let scanner = SourceScanner::new(&ctx.src_dir, &ctx.src_build_dir);
        let sources = scanner.scan_all(Some(&core_dir), Some(&variant_dir))?;

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
            include_dirs,
            mcu_config.clone(),
            params.profile,
            params.verbose,
        );

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

        // 8. Run shared sequential build pipeline
        pipeline::run_sequential_build(
            &compiler,
            &linker,
            ctx,
            params,
            &sources,
            TargetArchitecture::Arm,
            "SAM",
            start,
        )
    }
}

/// Install ArduinoCore-sam for classic SAM3X boards (Due).
///
/// Returns (framework_dir, core_dir, variant_dir, linker_script, system_includes).
fn install_sam_core(
    params: &BuildParams,
    core_name: &str,
    variant_name: &str,
) -> Result<(PathBuf, PathBuf, PathBuf, PathBuf, Vec<PathBuf>)> {
    let framework = fbuild_packages::library::SamCores::new(&params.project_dir);
    let framework_dir = fbuild_packages::Package::ensure_installed(&framework)?;
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
fn install_samd_core(
    params: &BuildParams,
    core_name: &str,
    variant_name: &str,
) -> Result<(PathBuf, PathBuf, PathBuf, PathBuf, Vec<PathBuf>)> {
    let framework = fbuild_packages::library::SamdCores::new(&params.project_dir);
    let framework_dir = fbuild_packages::Package::ensure_installed(&framework)?;
    tracing::info!("SAMD cores at {}", framework_dir.display());

    let core_dir = framework.get_core_dir(core_name);
    let variant_dir = framework.get_variant_dir(variant_name);
    let linker_script = framework.get_linker_script(variant_name);

    // SAMD core needs external CMSIS and CMSIS-Atmel packages for device headers
    let cmsis = fbuild_packages::library::CmsisFramework::new(&params.project_dir);
    let cmsis_dir = fbuild_packages::Package::ensure_installed(&cmsis)?;
    tracing::info!("CMSIS at {}", cmsis_dir.display());

    let cmsis_atmel = fbuild_packages::library::CmsisAtmel::new(&params.project_dir);
    let _cmsis_atmel_dir = fbuild_packages::Package::ensure_installed(&cmsis_atmel)?;
    tracing::info!("CMSIS-Atmel installed");

    let mut includes = vec![
        cmsis.get_core_include_dir(),
        cmsis.get_dsp_include_dir(),
        cmsis_atmel.get_device_include_dir(),
    ];
    // Also include the variant dir for variant.h
    includes.push(variant_dir.clone());

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
}
