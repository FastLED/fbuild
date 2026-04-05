//! SAM build orchestrator — wires together config, packages, compiler, linker.
//!
//! Build phases:
//! 1. Parse platformio.ini
//! 2. Load board config (due, etc.)
//! 3. Ensure ARM GCC toolchain
//! 4. Ensure SAM cores (ArduinoCore-sam)
//! 5. Setup build directories
//! 6. Scan source files
//! 7. Compile core sources
//! 8. Compile sketch sources
//! 9. Link (with linker script)
//! 10. Convert to binary + report size

use std::path::Path;
use std::time::Instant;

use fbuild_core::{Platform, Result};

use crate::compile_database::TargetArchitecture;
use crate::pipeline;
use crate::{BuildOrchestrator, BuildParams, BuildResult, SourceScanner};

use super::sam_compiler::SamCompiler;
use super::sam_linker::SamLinker;

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

        // 4. Ensure SAM cores (ArduinoCore-sam)
        let framework = fbuild_packages::library::SamCores::new(&params.project_dir);
        let framework_dir = fbuild_packages::Package::ensure_installed(&framework)?;
        tracing::info!("SAM cores at {}", framework_dir.display());

        // 5. Scan sources
        let core_dir = framework.get_core_dir(&ctx.board.core);
        let variant_dir = framework.get_variant_dir(&ctx.board.variant);

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
        let mut include_dirs = ctx.board.get_include_paths(&framework_dir);
        include_dirs.push(ctx.src_dir.clone());
        pipeline::discover_project_includes(&params.project_dir, &mut include_dirs);
        // Toolchain sysroot includes
        include_dirs.extend(toolchain.get_include_dirs());
        // SAM system includes (CMSIS)
        let system_dir = framework.get_system_dir();
        if system_dir.exists() {
            include_dirs.push(system_dir.join("libsam"));
            include_dirs.push(system_dir.join("CMSIS").join("CMSIS").join("Include"));
            include_dirs.push(system_dir.join("CMSIS").join("Device").join("ATMEL"));
        }

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
        let linker_script_path = framework.get_linker_script(&ctx.board.variant);
        let linker = SamLinker::new(
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
