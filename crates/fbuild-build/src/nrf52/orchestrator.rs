//! NRF52 build orchestrator — wires together config, packages, compiler, linker.
//!
//! Build phases:
//! 1. Parse platformio.ini
//! 2. Load board config (nrf52840_dk, etc.)
//! 3. Ensure ARM GCC toolchain
//! 4. Ensure NRF52 cores (Adafruit nRF52 Arduino core)
//! 5. Setup build directories
//! 6. Scan source files
//! 7. Compile core sources
//! 8. Compile sketch sources
//! 9. Link (with linker script)
//! 10. Convert to hex + report size

use std::path::Path;
use std::time::Instant;

use fbuild_core::{Platform, Result};

use crate::compile_database::TargetArchitecture;
use crate::pipeline;
use crate::{BuildOrchestrator, BuildParams, BuildResult, SourceScanner};

use super::nrf52_compiler::Nrf52Compiler;
use super::nrf52_linker::Nrf52Linker;

/// NRF52 platform build orchestrator.
pub struct Nrf52Orchestrator;

impl BuildOrchestrator for Nrf52Orchestrator {
    fn platform(&self) -> Platform {
        Platform::NordicNrf52
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

        // 4. Ensure NRF52 cores (Adafruit nRF52 Arduino core)
        let framework = fbuild_packages::library::Nrf52Cores::new(&params.project_dir);
        let framework_dir = fbuild_packages::Package::ensure_installed(&framework)?;
        tracing::info!("NRF52 cores at {}", framework_dir.display());

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
        let mcu_config = super::mcu_config::get_nrf52_config_for_mcu(&mcu_lower)?;
        let mut defines = ctx.board.get_defines();
        defines.extend(mcu_config.defines_map());
        let mut include_dirs = ctx.board.get_include_paths(&framework_dir);
        include_dirs.push(ctx.src_dir.clone());
        pipeline::discover_project_includes(&params.project_dir, &mut include_dirs);
        // Toolchain sysroot includes
        include_dirs.extend(toolchain.get_include_dirs());
        // CMSIS Core includes (core_cm4.h, etc.)
        let cmsis = fbuild_packages::library::CmsisFramework::new(&params.project_dir);
        let _cmsis_dir = fbuild_packages::Package::ensure_installed(&cmsis)?;
        tracing::info!("CMSIS framework installed");
        include_dirs.push(cmsis.get_core_include_dir());
        include_dirs.push(cmsis.get_dsp_include_dir());
        // Nordic SDK includes (bundled inside the core)
        let nordic_dir = core_dir.join("nordic");
        include_dirs.push(nordic_dir.clone());
        include_dirs.push(nordic_dir.join("nrfx"));
        include_dirs.push(nordic_dir.join("nrfx").join("hal"));
        include_dirs.push(nordic_dir.join("nrfx").join("mdk"));
        include_dirs.push(nordic_dir.join("nrfx").join("soc"));
        include_dirs.push(nordic_dir.join("nrfx").join("drivers").join("include"));
        include_dirs.push(nordic_dir.join("nrfx").join("drivers").join("src"));
        // SoftDevice API includes (s140 for nRF52840)
        let sd_dir = nordic_dir
            .join("softdevice")
            .join("s140_nrf52_6.1.1_API")
            .join("include");
        if sd_dir.exists() {
            include_dirs.push(sd_dir.clone());
            let sd_chip = sd_dir.join("nrf52");
            if sd_chip.exists() {
                include_dirs.push(sd_chip);
            }
        }
        // FreeRTOS includes
        let freertos = core_dir.join("freertos");
        include_dirs.push(freertos.join("Source").join("include"));
        include_dirs.push(freertos.join("config"));
        include_dirs.push(freertos.join("portable").join("GCC").join("nrf52"));
        include_dirs.push(freertos.join("portable").join("CMSIS").join("nrf52"));
        // SEGGER SystemView includes
        include_dirs.push(core_dir.join("sysview").join("SEGGER"));
        include_dirs.push(core_dir.join("sysview").join("Config"));
        // TinyUSB includes (USB CDC Serial support for nRF52840)
        let tinyusb_dir = framework_dir
            .join("libraries")
            .join("Adafruit_TinyUSB_Arduino")
            .join("src")
            .join("arduino");
        if tinyusb_dir.exists() {
            include_dirs.push(tinyusb_dir);
        }

        let compiler = Nrf52Compiler::new(
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
        let linker = Nrf52Linker::new(
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
            "NRF52",
            start,
        )
    }
}

/// Create an NRF52 orchestrator (convenience for get_orchestrator dispatch).
pub fn create() -> Box<dyn BuildOrchestrator> {
    Box::new(Nrf52Orchestrator)
}

/// Check if a project is configured for NRF52 by reading its platformio.ini.
pub fn is_nrf52_project(project_dir: &Path, env_name: &str) -> bool {
    crate::pipeline::is_platform_project(project_dir, env_name, fbuild_core::Platform::NordicNrf52)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nrf52_orchestrator_platform() {
        let orch = Nrf52Orchestrator;
        assert_eq!(orch.platform(), Platform::NordicNrf52);
    }
}
