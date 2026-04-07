//! STM32 build orchestrator — wires together config, packages, compiler, linker.
//!
//! Build phases:
//! 1. Parse platformio.ini
//! 2. Load board config (bluepill_f103c8, blackpill_f411ce, nucleo_*, etc.)
//! 3. Ensure ARM GCC toolchain
//! 4. Ensure STM32duino cores (Arduino_Core_STM32)
//! 5. Setup build directories
//! 6. Scan source files
//! 7. Compile core + variant sources
//! 8. Compile sketch sources
//! 9. Link (with linker script from variant dir)
//! 10. Convert to hex + report size

use std::path::Path;
use std::time::Instant;

use fbuild_core::{Platform, Result};
use fbuild_packages::Framework;

use crate::compile_database::TargetArchitecture;
use crate::generic_arm::{ArmCompiler, ArmLinker};
use crate::pipeline;
use crate::{BuildOrchestrator, BuildParams, BuildResult, SourceScanner};

/// STM32 platform build orchestrator.
pub struct Stm32Orchestrator;

impl BuildOrchestrator for Stm32Orchestrator {
    fn platform(&self) -> Platform {
        Platform::Ststm32
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

        // 4. Ensure STM32duino cores
        let framework = fbuild_packages::library::Stm32Cores::new(&params.project_dir);
        let framework_dir = fbuild_packages::Package::ensure_installed(&framework)?;
        tracing::info!("STM32 cores at {}", framework_dir.display());

        // 5. Scan sources (core + variant)
        // STM32duino uses "arduino" as its core directory name, even though
        // the board JSON says core = "stm32". Map it here.
        let core_dir = framework.get_core_dir("arduino");
        let variant_dir = framework.get_variant_dir(&ctx.board.variant);

        let scanner = SourceScanner::new(&ctx.src_dir, &ctx.src_build_dir);
        // Scan core + variant, but pass None for variant — we'll filter variant
        // sources manually because the variant dir contains files for multiple
        // board variants (MALYAN, AFROFLIGHT, etc.) and startup files that
        // conflict with the generic one in cores/arduino/stm32/.
        let mut sources = scanner.scan_all(Some(&core_dir), None)?;
        // Only include the generic variant files (not board-specific alternates)
        sources.variant_sources = scanner
            .scan_variant_sources(&variant_dir)
            .into_iter()
            .filter(|p| {
                let name = p
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_lowercase();
                // Keep: variant_generic.cpp, PeripheralPins.c, generic_clock.c
                // Skip: variant_MALYAN*.*, variant_AFRO*.*, startup_*.S, PeripheralPins_*.c
                (name.starts_with("variant_generic") || !name.starts_with("variant_"))
                    && !name.starts_with("peripheralpins_")
                    && !name.starts_with("startup_")
            })
            .collect();

        // SrcWrapper is a core library in STM32duino — its sources must be
        // compiled alongside the Arduino core (HAL wrappers, syscalls, etc.)
        // scan_core_sources is recursive, so one call covers all subdirs.
        let libs_dir = framework.get_libraries_dir();
        let srcwrapper_src = libs_dir.join("SrcWrapper").join("src");
        if srcwrapper_src.exists() {
            sources
                .core_sources
                .extend(scanner.scan_core_sources(&srcwrapper_src));
        }

        tracing::info!(
            "sources: {} sketch, {} core, {} variant",
            sources.sketch_sources.len(),
            sources.core_sources.len(),
            sources.variant_sources.len(),
        );

        // 6. Build include dirs + compiler
        // Extract MCU family from variant path (e.g. "STM32F1xx" from "STM32F1xx/F103C8T_...")
        let family = ctx.board.variant.split('/').next().unwrap_or("STM32F1xx");

        let mut mcu_config =
            super::mcu_config::get_stm32_config_for_mcu(&ctx.board.mcu.to_lowercase())?;
        // STM32duino linker scripts reference LD_MAX_SIZE, LD_MAX_DATA_SIZE, and
        // LD_FLASH_OFFSET as symbols. Provide them via --defsym from board config.
        let max_flash = ctx.board.max_flash.unwrap_or(65536);
        let max_ram = ctx.board.max_ram.unwrap_or(20480);
        mcu_config
            .linker_flags
            .push(format!("-Wl,--defsym=LD_MAX_SIZE={max_flash}"));
        mcu_config
            .linker_flags
            .push(format!("-Wl,--defsym=LD_MAX_DATA_SIZE={max_ram}"));
        mcu_config
            .linker_flags
            .push("-Wl,--defsym=LD_FLASH_OFFSET=0".to_string());
        let mut defines = ctx.board.get_defines();
        defines.extend(mcu_config.defines_map());
        // STM32duino's stm32_def.h checks for STM32YYxx (e.g. STM32F1xx). The board
        // JSON extra_flags may only have STM32F1, so ensure the full family define.
        defines.insert(family.to_string(), "1".to_string());
        // STM32duino variant sources are guarded by ARDUINO_GENERIC_<MCU> defines.
        // Derive from the MCU name: stm32f103c8t6 → ARDUINO_GENERIC_F103C8TX
        let generic_board = stm32_generic_board_define(&ctx.board.mcu);
        defines.insert(format!("ARDUINO_{generic_board}"), "1".to_string());
        // STM32duino requires these defines for HAL/LL and variant header resolution.
        defines.insert("USE_HAL_DRIVER".to_string(), "1".to_string());
        defines.insert("USE_FULL_LL_DRIVER".to_string(), "1".to_string());
        defines.insert(
            "VARIANT_H".to_string(),
            "\\\"variant_generic.h\\\"".to_string(),
        );
        // UART HAL module is disabled by default in stm32yyxx_hal_conf.h — enable it
        // so WSerial.h can create the Serial instance.
        defines.insert("HAL_UART_MODULE_ENABLED".to_string(), "1".to_string());
        defines.insert("HAL_PCD_MODULE_ENABLED".to_string(), "1".to_string());

        // Build include dirs manually (can't use get_include_paths because
        // STM32duino core dir is "arduino", not the board JSON's "stm32")
        let mut include_dirs = vec![core_dir.clone(), variant_dir.clone()];
        // Core subdirectories (AVR compat, STM32 HAL wrapper)
        include_dirs.push(core_dir.join("avr"));
        include_dirs.push(core_dir.join("stm32"));
        // SrcWrapper include dirs (clock.h, analog.h, LL/stm32yyxx_ll_*.h, etc.)
        let srcwrapper_inc = libs_dir.join("SrcWrapper").join("inc");
        if srcwrapper_inc.exists() {
            include_dirs.push(srcwrapper_inc.clone());
            let ll_inc = srcwrapper_inc.join("LL");
            if ll_inc.exists() {
                include_dirs.push(ll_inc);
            }
        }
        include_dirs.push(ctx.src_dir.clone());
        pipeline::discover_project_includes(&params.project_dir, &mut include_dirs);

        // STM32duino system includes (CMSIS device, HAL drivers, etc.)
        let system_dir = framework.get_system_dir();
        add_stm32_system_includes(&system_dir, family, &mut include_dirs);

        // CMSIS Core includes (core_cm3.h, core_cm4.h, etc.) — not bundled in STM32duino
        let cmsis = fbuild_packages::library::CmsisFramework::new(&params.project_dir);
        let _cmsis_dir = fbuild_packages::Package::ensure_installed(&cmsis)?;
        tracing::info!("CMSIS framework installed");
        include_dirs.push(cmsis.get_core_include_dir());

        // Toolchain sysroot includes (ARM CMSIS headers, etc.)
        include_dirs.extend(toolchain.get_include_dirs());

        let compiler = ArmCompiler::new(
            toolchain.get_gcc_path(),
            toolchain.get_gxx_path(),
            &ctx.board.mcu,
            &ctx.board.f_cpu,
            defines,
            include_dirs.clone(),
            mcu_config.clone(),
            params.profile,
            params.verbose,
        );

        // 7. Create linker (linker script from variant dir)
        let linker_script = match ctx.board.ldscript.as_deref() {
            Some(name) => variant_dir.join(name),
            None => framework.get_linker_script(&ctx.board.variant),
        };
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
        pipeline::run_sequential_build_with_libs(
            &compiler,
            &linker,
            ctx,
            params,
            &sources,
            Some(&lib_env),
            TargetArchitecture::Arm,
            "STM32",
            start,
        )
    }
}

/// Add STM32duino system include directories for CMSIS and HAL.
///
/// The STM32duino core bundles CMSIS and HAL drivers under `system/`:
/// - `system/Drivers/CMSIS/Device/ST/<family>/Include/` — MCU device headers
/// - `system/Drivers/CMSIS/Core/Include/` — ARM CMSIS core headers
/// - `system/Drivers/<family>_HAL_Driver/Inc/` — STM32 HAL headers
/// - `system/<family>/` — System startup headers
fn add_stm32_system_includes(
    system_dir: &Path,
    family: &str,
    include_dirs: &mut Vec<std::path::PathBuf>,
) {
    let drivers = system_dir.join("Drivers");

    // CMSIS Device headers (stm32f1xx.h, stm32f103xb.h, etc.)
    let cmsis_device = drivers
        .join("CMSIS")
        .join("Device")
        .join("ST")
        .join(family)
        .join("Include");
    if cmsis_device.exists() {
        include_dirs.push(cmsis_device);
    }

    // CMSIS Core headers (core_cm3.h, core_cm4.h, etc.)
    let cmsis_core = drivers.join("CMSIS").join("Core").join("Include");
    if cmsis_core.exists() {
        include_dirs.push(cmsis_core);
    }

    // CMSIS startup templates (startup_stm32f103xb.s, etc.)
    let cmsis_startup = drivers
        .join("CMSIS")
        .join("Device")
        .join("ST")
        .join(family)
        .join("Source")
        .join("Templates")
        .join("gcc");
    if cmsis_startup.exists() {
        include_dirs.push(cmsis_startup);
    }

    // HAL Driver headers (stm32f1xx_hal.h, etc.)
    let hal_driver = drivers.join(format!("{family}_HAL_Driver"));
    let hal_inc = hal_driver.join("Inc");
    if hal_inc.exists() {
        include_dirs.push(hal_inc);
    }
    // HAL Driver sources — SrcWrapper's stm32yyxx_hal.c does #include "stm32f1xx_hal.c"
    let hal_src = hal_driver.join("Src");
    if hal_src.exists() {
        include_dirs.push(hal_src);
    }

    // System family directory (startup and system config headers)
    let system_family = system_dir.join(family);
    if system_family.exists() {
        include_dirs.push(system_family);
    }
}

/// Derive the STM32duino generic board define from the MCU name.
///
/// `stm32f103c8t6` → `GENERIC_F103C8TX`
/// `stm32f411ceu6` → `GENERIC_F411CEUX`
///
/// Pattern: strip `stm32` prefix, uppercase, replace last char with `X`.
fn stm32_generic_board_define(mcu: &str) -> String {
    let suffix = mcu
        .to_lowercase()
        .strip_prefix("stm32")
        .unwrap_or(&mcu.to_lowercase())
        .to_uppercase();
    // Replace last character (pin-count digit) with X
    let mut chars: Vec<char> = suffix.chars().collect();
    if let Some(last) = chars.last_mut() {
        *last = 'X';
    }
    let trimmed: String = chars.into_iter().collect();
    format!("GENERIC_{trimmed}")
}

/// Create an STM32 orchestrator.
pub fn create() -> Box<dyn BuildOrchestrator> {
    Box::new(Stm32Orchestrator)
}

/// Check if a project is configured for STM32.
pub fn is_stm32_project(project_dir: &Path, env_name: &str) -> bool {
    crate::pipeline::is_platform_project(project_dir, env_name, fbuild_core::Platform::Ststm32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stm32_orchestrator_platform() {
        let orch = Stm32Orchestrator;
        assert_eq!(orch.platform(), Platform::Ststm32);
    }

    #[test]
    fn test_is_stm32_project() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:bluepill]\nplatform = ststm32\nboard = bluepill_f103c8\nframework = arduino\n",
        )
        .unwrap();
        assert!(is_stm32_project(tmp.path(), "bluepill"));
        assert!(!is_stm32_project(tmp.path(), "uno"));
    }
}
