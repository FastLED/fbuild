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
use serde::Serialize;

use crate::build_fingerprint::{
    expected_fast_path_artifacts, stable_hash_json, FastPathCheckInputs, FastPathContract,
    FastPathPersistInputs, BUILD_FINGERPRINT_VERSION,
};
use crate::compile_database::TargetArchitecture;
use crate::pipeline;
use crate::{BuildOrchestrator, BuildParams, BuildResult, SourceScanner};

use super::nrf52_compiler::Nrf52Compiler;
use super::nrf52_linker::Nrf52Linker;

/// NRF52 platform build orchestrator.
pub struct Nrf52Orchestrator;

#[derive(Debug, Serialize)]
struct Nrf52FingerprintMetadata {
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
    linker_script: String,
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

impl BuildOrchestrator for Nrf52Orchestrator {
    fn platform(&self) -> Platform {
        Platform::NordicNrf52
    }

    fn build(&self, params: &BuildParams) -> Result<BuildResult> {
        let start = Instant::now();
        let compiler_cache = crate::zccache::find_zccache().map(std::path::Path::to_path_buf);

        // 1-2. Parse config, load board, setup build dirs, resolve src dir, collect flags
        let mut ctx = pipeline::BuildContext::new(params)?;

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

        let build_dir = &ctx.build_dir;
        let ldscript_name = ctx
            .board
            .ldscript
            .as_deref()
            .unwrap_or("nrf52840_s140_v6.ld");
        let metadata_hash = stable_hash_json(&Nrf52FingerprintMetadata {
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
            linker_script: ldscript_name.to_string(),
            platform: "nordicnrf52".to_string(),
            max_flash: ctx.board.max_flash,
            max_ram: ctx.board.max_ram,
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
                    "No-op fingerprint matched; reusing existing NRF52 artifacts.".to_string(),
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
                        "NRF52 ({}) build for {} reused cached artifacts",
                        ctx.board.mcu, params.env_name
                    ),
                    compile_database_path: Some(fast_compile_db),
                    build_log: ctx.build_log,
                });
            }
        }

        // 5. Scan sources
        let core_dir = framework.get_core_dir(&ctx.board.core);
        let variant_dir = framework.get_variant_dir(&ctx.board.variant);

        let scanner = SourceScanner::new(&ctx.src_dir, &ctx.src_build_dir);
        let mut sources = scanner.scan_all_filtered(
            Some(&core_dir),
            Some(&variant_dir),
            ctx.source_filter.as_deref(),
        )?;

        // Add TinyUSB sources (USB CDC Serial support for nRF52840).
        // Compile arduino/ wrapper + device stack + CDC class + nRF5x port.
        let tinyusb_root = framework_dir
            .join("libraries")
            .join("Adafruit_TinyUSB_Arduino")
            .join("src");
        if tinyusb_root.exists() {
            for subdir in &[
                "arduino",
                "device",
                "common",
                "class/cdc",
                "class/vendor",
                "class/msc",
                "class/hid",
                "class/midi",
                "class/video",
                "class/audio",
                "class/dfu",
                "class/net",
                "class/usbtmc",
                "portable/nordic/nrf5x",
            ] {
                let dir = tinyusb_root.join(subdir);
                if dir.exists() {
                    sources.core_sources.extend(scanner.scan_core_sources(&dir));
                }
            }
            // tusb.c at the root
            let tusb_c = tinyusb_root.join("tusb.c");
            if tusb_c.exists() {
                sources.core_sources.push(tusb_c);
            }
            tracing::info!(
                "TinyUSB sources added to core (total core: {})",
                sources.core_sources.len()
            );
        }

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
        // Reuse the alias-resolved core_dir/variant_dir computed at step 5.
        // BoardConfig::get_include_paths joins variants/<self.variant> literally
        // and would emit a non-existent path like variants/nRF52DK/ when the
        // board JSON uses a PIO/sandeepmistry variant name that aliases to a
        // different Adafruit directory (e.g. nRF52DK -> pca10056, see #321).
        // That would surface as `fatal error: variant.h: No such file or
        // directory` when compiling cores/nRF5/{Uart,delay}.h.
        let mut include_dirs = vec![core_dir.clone(), variant_dir.clone()];
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
        let tinyusb_src = framework_dir
            .join("libraries")
            .join("Adafruit_TinyUSB_Arduino")
            .join("src");
        if tinyusb_src.exists() {
            include_dirs.push(tinyusb_src.join("arduino"));
            include_dirs.push(tinyusb_src.clone());
        }
        // Framework library includes (SPI, Wire, etc.)
        let libs_dir = framework_dir.join("libraries");
        for lib_name in &["SPI", "Wire"] {
            let lib_dir = libs_dir.join(lib_name);
            if lib_dir.exists() {
                include_dirs.push(lib_dir);
            }
        }

        let compiler = Nrf52Compiler::new(
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

        // 7. Create linker (resolve linker script from board config)
        let linker_script_path = framework.get_linker_script(ldscript_name);
        let linker = Nrf52Linker::new(
            toolchain.get_gcc_path(),
            toolchain.get_ar_path(),
            toolchain.get_objcopy_path(),
            toolchain.get_size_path(),
            linker_script_path,
            vec![framework.get_linker_dir()],
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
            "NRF52",
            start,
        )?;

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

    #[test]
    fn test_fast_path_contract_includes_project_and_resolved_libs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let build_dir = tmp.path().join("build");
        let project_dir = tmp.path().join("project");
        let libs_dir = build_dir.join("libs");
        std::fs::create_dir_all(&libs_dir).unwrap();
        std::fs::create_dir_all(&project_dir).unwrap();

        let contract = FastPathContract::for_project_outputs(
            &build_dir,
            &project_dir,
            Vec::<std::path::PathBuf>::new(),
        );

        assert_eq!(contract.watches().len(), 2);
        assert_eq!(contract.watches()[0].root, project_dir);
        assert_eq!(contract.watches()[1].root, libs_dir);
    }
}
