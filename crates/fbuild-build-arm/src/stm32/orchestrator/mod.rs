//! STM32 build orchestrator â€” wires together config, packages, compiler, linker.
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
//!
//! Module layout (refactored to keep each .rs file under the 1000-LOC gate):
//! - `arduino_mbed` â€” Arduino mbed-core build path (GIGA, PORTENTA, ...)
//! - `framework_props` â€” STM32duino `boards.txt` parser
//! - `includes` â€” include-path/define helpers and small shared utilities
//! - `variant_files` â€” variant_*.{h,cpp} / PeripheralPins_*.c selection
//!
//! All four submodules are private internals of the STM32 orchestrator.

mod arduino_mbed;
mod framework_props;
mod includes;
mod variant_files;

use std::path::Path;
use std::time::Instant;

use fbuild_core::{Platform, Result};
use fbuild_packages::{Framework, Toolchain};

use crate::compile_database::TargetArchitecture;
use crate::framework_libs::{
    library_select_kv_store, resolve_framework_library_sources_active,
    resolve_framework_library_sources_cached,
};
use crate::generic_arm::{ArmCompiler, ArmLinker};
use crate::pipeline;
use crate::{BuildOrchestrator, BuildParams, BuildResult, SourceScanner};

use self::arduino_mbed::{build_arduino_mbed_stm32, is_arduino_mbed_stm32_variant};
use self::framework_props::load_stm32_framework_props;
use self::includes::{add_stm32_system_includes, stm32_generic_board_define};
use self::variant_files::{keep_variant_source, select_variant_files};

/// STM32 platform build orchestrator.
pub struct Stm32Orchestrator;

#[async_trait::async_trait]
impl BuildOrchestrator for Stm32Orchestrator {
    fn platform(&self) -> Platform {
        Platform::Ststm32
    }

    async fn build(&self, params: &BuildParams) -> Result<BuildResult> {
        let start = Instant::now();

        // 1-2. Parse config, load board, setup build dirs, resolve src dir, collect flags
        let mut ctx = pipeline::BuildContext::new(params).await?;

        // Compute eh_frame strip policy once per build (FastLED/fbuild#244).
        let eh_frame_policy =
            crate::eh_frame_policy_compute::compute_eh_frame_policy(&ctx, params.profile, None);

        // 3. Ensure ARM GCC toolchain
        let toolchain = fbuild_packages::toolchain::ArmToolchain::new(&params.project_dir);
        let toolchain_dir = fbuild_packages::Package::ensure_installed(&toolchain).await?;
        tracing::info!("arm-gcc toolchain at {}", toolchain_dir.display());

        pipeline::log_toolchain_version(
            &toolchain.get_gcc_path(),
            "arm-none-eabi-gcc",
            &mut ctx.build_log,
        )
        .await;

        if is_arduino_mbed_stm32_variant(&ctx.board.variant) {
            return build_arduino_mbed_stm32(params, ctx, &toolchain, start).await;
        }

        // 4. Ensure STM32duino cores
        // Honor `platform_packages` override (FastLED/fbuild#664, #681).
        let __ovr = ctx
            .config
            .get_env_config(&params.env_name)
            .ok()
            .and_then(|env| {
                crate::package_override::resolve_override(env, "framework-arduinoststm32")
            });
        let framework = match __ovr {
            Some(o) => fbuild_packages::library::Stm32Cores::with_override(&params.project_dir, o),
            None => fbuild_packages::library::Stm32Cores::new(&params.project_dir),
        };
        let framework_dir = fbuild_packages::Package::ensure_installed(&framework).await?;
        tracing::info!("STM32 cores at {}", framework_dir.display());

        // 5. Scan sources (core + variant)
        // STM32duino uses "arduino" as its core directory name, even though
        // the board JSON says core = "stm32". Map it here.
        let core_dir = framework.get_core_dir("arduino");
        let framework_props =
            load_stm32_framework_props(&ctx.board.variant, &framework.get_boards_txt());
        let resolved_variant = framework_props
            .as_ref()
            .and_then(|props| props.get("variant").cloned())
            .unwrap_or_else(|| ctx.board.variant.clone());
        let variant_dir = framework.get_variant_dir(&resolved_variant);
        let selected_variant = select_variant_files(
            &variant_dir,
            &resolved_variant,
            framework_props
                .as_ref()
                .and_then(|props| props.get("variant_h").map(String::as_str))
                .or(ctx.board.variant_h.as_deref()),
        );

        let scanner = SourceScanner::new(&ctx.src_dir, &ctx.src_build_dir);
        // Scan core + variant, but pass None for variant â€” we'll filter variant
        // sources manually because the variant dir contains files for multiple
        // board variants (MALYAN, AFROFLIGHT, etc.) and startup files that
        // conflict with the generic one in cores/arduino/stm32/.
        let mut sources =
            scanner.scan_all_filtered(Some(&core_dir), None, ctx.source_filter.as_deref())?;
        sources.variant_sources = scanner
            .scan_variant_sources(&variant_dir)
            .into_iter()
            .filter(|p| keep_variant_source(p, &selected_variant))
            .collect();

        // SrcWrapper is a core library in STM32duino â€” its sources must be
        // compiled alongside the Arduino core (HAL wrappers, syscalls, etc.)
        // scan_core_sources is recursive, so one call covers all subdirs.
        let libs_dir = framework.get_libraries_dir();
        let srcwrapper_src = libs_dir.join("SrcWrapper").join("src");
        if srcwrapper_src.exists() {
            sources
                .core_sources
                .extend(scanner.scan_core_sources(&srcwrapper_src));
        }

        // Walk Arduino_Core_STM32's libraries/ (SPI, Wire, EEPROM, ...) and
        // pull in any the sketch transitively #includes. Without this, sketches
        // that include <SPI.h> fail with "No such file or directory" because
        // STM32duino only exposes bundled libraries via this framework-level
        // discovery (PlatformIO's LDF does the same for `framework = arduino`).
        let framework_libs = framework.get_framework_libraries();
        let ldf_mcu_config =
            super::mcu_config::get_stm32_config_for_mcu(&ctx.board.mcu.to_lowercase())?;
        let mut ldf_defines = ctx.board.get_defines();
        ldf_defines.extend(ldf_mcu_config.defines_map());
        // WHY: STM32duino targets every Cortex-M family from M0 (F0xx) up
        // through M7 (H7xx) but the toolchain triple is constant
        // (`arm-none-eabi`). The cache key already includes
        // `framework_install_path` + `framework_version`, so per-MCU drift
        // is handled there â€” this string only needs to disambiguate stm32
        // from teensy etc. so cross-platform key collisions are impossible.
        let framework_info = fbuild_packages::Package::get_info(&framework);
        let framework_library_sources = match library_select_kv_store() {
            Some(store) => {
                let key_inputs = fbuild_library_select::cache::CacheKeyInputs {
                    toolchain_triple: "stm32-arm-none-eabi",
                    framework_install_path: &framework_info.install_path,
                    framework_version: &framework_info.version,
                    preprocessor_defines: &ldf_defines,
                };
                resolve_framework_library_sources_cached(
                    &framework_libs,
                    &params.project_dir,
                    &ctx.src_dir,
                    &key_inputs,
                    store,
                )
            }
            None => resolve_framework_library_sources_active(
                &framework_libs,
                &params.project_dir,
                &ctx.src_dir,
                &ldf_defines,
            ),
        };
        if !framework_library_sources.is_empty() {
            tracing::info!(
                "STM32 framework library sources added: {}",
                framework_library_sources.len()
            );
            sources.core_sources.extend(framework_library_sources);
        }

        tracing::info!(
            "sources: {} sketch, {} core, {} variant",
            sources.sketch_sources.len(),
            sources.core_sources.len(),
            sources.variant_sources.len(),
        );

        // 6. Build include dirs + compiler
        // Extract MCU family from variant path (e.g. "STM32F1xx" from "STM32F1xx/F103C8T_...")
        let family = resolved_variant.split('/').next().unwrap_or("STM32F1xx");

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
        if let Some(board_define) = framework_props
            .as_ref()
            .and_then(|props| props.get("board"))
        {
            defines.insert(format!("ARDUINO_{board_define}"), "1".to_string());
        }
        // STM32duino's stm32_def.h checks for STM32YYxx (e.g. STM32F1xx). The board
        // JSON extra_flags may only have STM32F1, so ensure the full family define.
        defines.insert(family.to_string(), "1".to_string());
        // STM32duino variant sources are guarded by ARDUINO_GENERIC_<MCU> defines.
        // Derive from the MCU name: stm32f103c8t6 â†’ ARDUINO_GENERIC_F103C8TX
        let generic_board = stm32_generic_board_define(&ctx.board.mcu);
        defines.insert(format!("ARDUINO_{generic_board}"), "1".to_string());
        // STM32duino requires these defines for HAL/LL and variant header resolution.
        defines.insert("USE_HAL_DRIVER".to_string(), "1".to_string());
        defines.insert("USE_FULL_LL_DRIVER".to_string(), "1".to_string());
        defines.insert(
            "VARIANT_H".to_string(),
            format!("\\\"{}\\\"", selected_variant.header),
        );
        // UART HAL module is disabled by default in stm32yyxx_hal_conf.h â€” enable it
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
        // Bundled framework library headers (SPI, Wire, EEPROM, ...) so
        // sketches can `#include <SPI.h>` etc.
        include_dirs.extend(framework.get_framework_library_include_dirs());

        // STM32duino system includes (CMSIS device, HAL drivers, etc.)
        let system_dir = framework.get_system_dir();
        add_stm32_system_includes(&system_dir, family, &mut include_dirs);

        // CMSIS Core includes (core_cm3.h, core_cm4.h, etc.) â€” not bundled in STM32duino
        let cmsis = fbuild_packages::library::CmsisFramework::new(&params.project_dir);
        let _cmsis_dir = fbuild_packages::Package::ensure_installed(&cmsis).await?;
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
        )
        .with_build_unflags(ctx.build_unflags.clone())
        .with_eh_frame_policy(eh_frame_policy);

        // 7. Create linker (linker script from variant dir)
        let linker_script = match ctx.board.ldscript.as_deref() {
            Some(name) => variant_dir.join(name),
            None => framework.get_linker_script(&resolved_variant),
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
            &[],
            Some(&lib_env),
            TargetArchitecture::Arm,
            "STM32",
            start,
        )
        .await
    }
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
    use super::framework_props::{load_stm32_framework_props, stm32_property_scopes};
    use super::variant_files::select_variant_files;
    use super::*;
    use std::fs;

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

    #[test]
    fn test_load_stm32_framework_props_resolves_variant_h_template() {
        let tmp = tempfile::TempDir::new().unwrap();
        let boards_txt = tmp.path().join("boards.txt");
        fs::write(
            &boards_txt,
            "\
GenF1.build.variant_h=variant_{build.board}.h
GenF1.menu.pnum.MAPLEMINI_F103CB.build.board=MAPLEMINI_F103CB
GenF1.menu.pnum.MAPLEMINI_F103CB.build.variant=STM32F1xx/F103C8T_F103CB(T-U)
giga.menu.target_core.cm7.build.variant=GIGA
giga.build.extra_ldflags=-DCM4_BINARY_START=0x08180000
",
        )
        .unwrap();

        let maple = load_stm32_framework_props("MAPLEMINI_F103CB", &boards_txt).unwrap();
        assert_eq!(
            maple.get("variant_h").map(String::as_str),
            Some("variant_MAPLEMINI_F103CB.h")
        );

        let giga = load_stm32_framework_props("GIGA", &boards_txt).unwrap();
        assert_eq!(
            giga.get("extra_ldflags").map(String::as_str),
            Some("-DCM4_BINARY_START=0x08180000")
        );
    }

    #[test]
    fn test_stm32_property_scopes_include_parent_menu_levels() {
        assert_eq!(
            stm32_property_scopes("giga.menu.target_core.cm7"),
            vec!["giga", "giga.menu.target_core.cm7"]
        );
        assert_eq!(
            stm32_property_scopes("foo.menu.cpu.atmega328.menu.speed.fast"),
            vec![
                "foo",
                "foo.menu.cpu.atmega328",
                "foo.menu.cpu.atmega328.menu.speed.fast"
            ]
        );
    }

    #[test]
    fn test_select_variant_files_prefers_framework_header() {
        let tmp = tempfile::TempDir::new().unwrap();
        fs::write(tmp.path().join("variant_MAPLEMINI_F103CB.h"), "").unwrap();
        fs::write(tmp.path().join("variant_MAPLEMINI_F103CB.cpp"), "").unwrap();
        fs::write(tmp.path().join("variant_generic.cpp"), "").unwrap();

        let selected = select_variant_files(
            tmp.path(),
            "STM32F1xx/F103C8T_F103CB(T-U)",
            Some("variant_MAPLEMINI_F103CB.h"),
        );

        assert_eq!(selected.header, "variant_MAPLEMINI_F103CB.h");
        assert_eq!(
            selected.source_stem.as_deref(),
            Some("variant_maplemini_f103cb")
        );
    }
}
