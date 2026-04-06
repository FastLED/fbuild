//! RP2040/RP2350 build orchestrator — wires together config, packages, compiler, linker.
//!
//! Build phases:
//! 1. Parse platformio.ini
//! 2. Load board config (rpipico, rpipico2, etc.)
//! 3. Ensure ARM GCC toolchain
//! 4. Ensure RP2040 cores (arduino-pico by earlephilhower)
//! 5. Setup build directories
//! 6. Scan source files
//! 7. Compile core + variant sources
//! 8. Compile sketch sources
//! 9. Link (with linker script from variant dir)
//! 10. Convert to binary + report size

use std::path::Path;
use std::time::Instant;

use fbuild_core::{Platform, Result};

use crate::compile_database::TargetArchitecture;
use crate::generic_arm::{ArmCompiler, ArmLinker};
use crate::pipeline;
use crate::{BuildOrchestrator, BuildParams, BuildResult, SourceScanner};

/// RP2040 platform build orchestrator.
pub struct Rp2040Orchestrator;

impl BuildOrchestrator for Rp2040Orchestrator {
    fn platform(&self) -> Platform {
        Platform::RaspberryPi
    }

    fn build(&self, params: &BuildParams) -> Result<BuildResult> {
        let start = Instant::now();

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

        // 4. Ensure RP2040 cores (arduino-pico by earlephilhower)
        let framework = fbuild_packages::library::Rp2040Cores::new(&params.project_dir);
        let framework_dir = fbuild_packages::Package::ensure_installed(&framework)?;
        tracing::info!("RP2040 cores at {}", framework_dir.display());

        // 5. Scan sources (core + variant)
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
        let mcu_config =
            super::mcu_config::get_rp2040_config_for_mcu(&ctx.board.mcu.to_lowercase())?;
        let mut defines = ctx.board.get_defines();
        defines.extend(mcu_config.defines_map());
        let mut include_dirs = ctx.board.get_include_paths(&framework_dir);
        include_dirs.push(ctx.src_dir.clone());
        pipeline::discover_project_includes(&params.project_dir, &mut include_dirs);
        // Toolchain sysroot includes
        include_dirs.extend(toolchain.get_include_dirs());
        // Pico SDK includes
        let pico_sdk_dir = framework.get_pico_sdk_dir();
        let pico_sdk_src = pico_sdk_dir.join("src");
        if pico_sdk_src.exists() {
            // Common headers (pico.h, pico/types.h, etc.)
            let common_inc = pico_sdk_src
                .join("common")
                .join("pico_base_headers")
                .join("include");
            if common_inc.exists() {
                include_dirs.push(common_inc);
            }
            // Board headers
            let boards_inc = pico_sdk_src.join("boards").join("include");
            if boards_inc.exists() {
                include_dirs.push(boards_inc);
            }
        }

        let compiler = ArmCompiler::new(
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

        // 7. Create linker (linker script from variant dir)
        let linker_script = framework.get_linker_script(&ctx.board.variant);
        let linker = ArmLinker::new(
            toolchain.get_gcc_path(),
            toolchain.get_ar_path(),
            toolchain.get_objcopy_path(),
            toolchain.get_size_path(),
            linker_script,
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
            "RP2040",
            start,
        )
    }
}

/// Create an RP2040 orchestrator.
pub fn create() -> Box<dyn BuildOrchestrator> {
    Box::new(Rp2040Orchestrator)
}

/// Check if a project is configured for RP2040.
pub fn is_rp2040_project(project_dir: &Path, env_name: &str) -> bool {
    crate::pipeline::is_platform_project(project_dir, env_name, fbuild_core::Platform::RaspberryPi)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rp2040_orchestrator_platform() {
        let orch = Rp2040Orchestrator;
        assert_eq!(orch.platform(), Platform::RaspberryPi);
    }
}
