//! ESP8266 build orchestrator — wires together config, packages, compiler, linker.
//!
//! Build phases:
//! 1. Parse platformio.ini
//! 2. Load board config
//! 3. Ensure xtensa-lx106-elf toolchain
//! 4. Ensure Arduino ESP8266 framework
//! 5. Load MCU config from embedded JSON
//! 6. Scan source files
//! 7. Build include dirs + compiler + linker
//! 8. Run shared sequential build pipeline

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use fbuild_core::{Platform, Result};
use fbuild_packages::Framework as _;

use crate::compile_database::TargetArchitecture;
use crate::pipeline;
use crate::{BuildOrchestrator, BuildParams, BuildResult, SourceScanner};

use super::esp8266_compiler::Esp8266Compiler;
use super::esp8266_linker::Esp8266Linker;
use super::mcu_config::get_esp8266_config;

/// ESP8266 platform build orchestrator.
pub struct Esp8266Orchestrator;

impl BuildOrchestrator for Esp8266Orchestrator {
    fn platform(&self) -> Platform {
        Platform::Espressif8266
    }

    fn build(&self, params: &BuildParams) -> Result<BuildResult> {
        let start = Instant::now();

        // 1-2. Parse config, load board, setup build dirs, resolve src dir, collect flags
        let mut ctx = pipeline::BuildContext::new(params)?;

        // 3. Ensure toolchain
        let toolchain = fbuild_packages::toolchain::Esp8266Toolchain::new(&params.project_dir);
        let _toolchain_dir = fbuild_packages::Package::ensure_installed(&toolchain)?;
        tracing::info!("ESP8266 toolchain ready");

        use fbuild_packages::Toolchain as _;
        pipeline::log_toolchain_version(
            &toolchain.get_gcc_path(),
            "xtensa-lx106-elf-gcc",
            &mut ctx.build_log,
        );

        // 4. Ensure framework
        let framework = fbuild_packages::library::Esp8266Framework::new(&params.project_dir);
        let _framework_dir = fbuild_packages::Package::ensure_installed(&framework)?;
        tracing::info!("ESP8266 framework ready");
        let board_id = ctx
            .config
            .get_env_config(&params.env_name)?
            .get("board")
            .cloned()
            .unwrap_or_default();
        let board_props = crate::arduino_props::load_board_props_with_default_menus(
            &framework.get_boards_txt(),
            &board_id,
        );

        let core_dir = framework.get_core_dir(&ctx.board.core);
        let variant_dir = framework.get_variant_dir(&ctx.board.variant);

        // 5. Load MCU config
        let mut mcu_config = get_esp8266_config()?;
        apply_esp8266_board_props(&board_props, &mut mcu_config);

        // 6. Scan sources
        let scanner = SourceScanner::new(&ctx.src_dir, &ctx.src_build_dir);
        let variant_dir_opt = if variant_dir.exists() {
            Some(variant_dir.as_path())
        } else {
            None
        };
        let sources = scanner.scan_all_filtered(
            Some(&core_dir),
            variant_dir_opt,
            ctx.source_filter.as_deref(),
        )?;

        tracing::info!(
            "sources: {} sketch, {} core, {} variant",
            sources.sketch_sources.len(),
            sources.core_sources.len(),
            sources.variant_sources.len(),
        );

        // 7. Build include dirs + defines
        let mut defines = ctx.board.get_defines();
        apply_define_flags_from_props(&board_props, &mut defines);
        apply_esp8266_board_identity(&board_props, &board_id, &mut defines);
        defines.extend(mcu_config.defines_map());
        let mut include_dirs = vec![core_dir.clone()];
        if variant_dir.exists() {
            include_dirs.push(variant_dir.clone());
        }
        // SDK include paths
        include_dirs.extend(framework.get_sdk_include_dirs());
        // Toolchain sysroot includes (xtensa/coreasm.h, etc.)
        // Required by .S assembly files — see platform.txt compiler.S.flags.
        include_dirs.extend(toolchain.get_include_dirs());
        // SDK libc headers (platform.txt compiler.libc.path)
        include_dirs.extend(framework.get_libc_include_dirs());
        // Built-in Arduino libraries (ESP8266WiFi, etc.)
        let builtin_libs_dir = framework.get_libraries_dir();
        if builtin_libs_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&builtin_libs_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        let lib_src = path.join("src");
                        if lib_src.is_dir() {
                            include_dirs.push(lib_src);
                        }
                    }
                }
            }
        }
        include_dirs.push(ctx.src_dir.clone());
        pipeline::discover_project_includes(&params.project_dir, &mut include_dirs);

        let compiler = Esp8266Compiler::new(
            toolchain.get_gcc_path(),
            toolchain.get_gxx_path(),
            &ctx.board.f_cpu,
            defines,
            include_dirs.clone(),
            mcu_config.clone(),
            params.profile,
            params.verbose,
        )
        .with_build_unflags(ctx.build_unflags.clone());

        // Resolve linker script from board config
        let ldscript = ctx
            .board
            .ldscript
            .as_deref()
            .unwrap_or("eagle.flash.4m1m.ld");
        let sdk_ld_dir = framework.get_sdk_ld_dir();
        let linker_scripts = crate::linker::LinkerScripts::single(sdk_ld_dir.clone(), ldscript);

        // Prefer f_image over f_flash for esptool frequency (see ESP32 orchestrator comment)
        let f_for_image = ctx
            .board
            .f_image
            .as_deref()
            .or(ctx.board.f_flash.as_deref());
        let flash_freq = crate::esp32::esp32_linker::f_flash_to_esptool_freq(
            f_for_image,
            &mcu_config.esptool.default_flash_freq,
        );

        let sdk_name = esp8266_sdk_name(&mcu_config).to_string();
        let linker = Esp8266Linker::new(
            toolchain.get_gcc_path(),
            toolchain.get_ar_path(),
            toolchain.get_objcopy_path(),
            toolchain.get_size_path(),
            framework.get_sdk_lib_dir(),
            framework.get_sdk_nonosdk_lib_dir_for(&sdk_name),
            sdk_ld_dir,
            linker_scripts,
            mcu_config,
            params.profile,
            ctx.board.flash_mode.clone(),
            &flash_freq,
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
            TargetArchitecture::Xtensa,
            "ESP8266",
            start,
        )
    }
}

fn apply_define_flags_from_props(
    board_props: &Option<HashMap<String, String>>,
    defines: &mut HashMap<String, String>,
) {
    let Some(props) = board_props.as_ref() else {
        return;
    };
    for key in [
        "flash_flags",
        "lwip_flags",
        "mmuflags",
        "debug_port",
        "debug_level",
        "vtable_flags",
    ] {
        if let Some(flags) = props.get(key) {
            let tokens = fbuild_core::shell_split::split(flags);
            for token in tokens {
                if let Some(def) = token.strip_prefix("-D") {
                    if let Some((name, value)) = def.split_once('=') {
                        defines.insert(name.to_string(), value.to_string());
                    } else {
                        defines.insert(def.to_string(), "1".to_string());
                    }
                }
            }
        }
    }
}

fn apply_esp8266_board_props(
    board_props: &Option<HashMap<String, String>>,
    mcu_config: &mut super::mcu_config::Esp8266McuConfig,
) {
    let Some(props) = board_props.as_ref() else {
        return;
    };

    if let Some(sdk_name) = props.get("sdk") {
        mcu_config
            .defines
            .retain(|entry| !matches!(entry, crate::esp32::mcu_config::DefineEntry::KeyValue(name, _) if name.starts_with("NONOSDK")));
        mcu_config
            .defines
            .push(crate::esp32::mcu_config::DefineEntry::KeyValue(
                sdk_name.clone(),
                "1".to_string(),
            ));
    }

    for key in ["flash_flags", "lwip_flags", "mmuflags", "vtable_flags"] {
        if let Some(flags) = props.get(key) {
            for token in fbuild_core::shell_split::split(flags) {
                if let Some(def) = token.strip_prefix("-D") {
                    let (name, value) = def
                        .split_once('=')
                        .map(|(name, value)| (name.to_string(), value.to_string()))
                        .unwrap_or_else(|| (def.to_string(), "1".to_string()));
                    mcu_config.defines.retain(|entry| match entry {
                        crate::esp32::mcu_config::DefineEntry::Simple(existing) => {
                            existing != &name
                        }
                        crate::esp32::mcu_config::DefineEntry::KeyValue(existing, _) => {
                            existing != &name
                        }
                    });
                    mcu_config
                        .defines
                        .push(crate::esp32::mcu_config::DefineEntry::KeyValue(name, value));
                }
            }
        }
    }

    if let Some(lwip_lib) = props.get("lwip_lib") {
        for lib in &mut mcu_config.linker_libs {
            if lib.starts_with("-llwip") {
                *lib = lwip_lib.clone();
                break;
            }
        }
    }
    if let Some(stdcpp_lib) = props.get("stdcpp_lib") {
        for lib in &mut mcu_config.linker_libs {
            if lib == "-lstdc++" || lib == "-lstdc++-exc" {
                *lib = stdcpp_lib.clone();
                break;
            }
        }
    }
}

fn apply_esp8266_board_identity(
    board_props: &Option<HashMap<String, String>>,
    board_id: &str,
    defines: &mut HashMap<String, String>,
) {
    if let Some(props) = board_props.as_ref() {
        if let Some(board_define) = props.get("board") {
            defines.insert(
                format!("ARDUINO_{}", board_define.to_uppercase()),
                "1".to_string(),
            );
        }
    }

    defines.insert(
        "ARDUINO_BOARD".to_string(),
        format!("\\\"PLATFORMIO_{}\\\"", board_id.to_uppercase()),
    );
    defines.insert(
        "ARDUINO_BOARD_ID".to_string(),
        format!("\\\"{}\\\"", board_id),
    );
}

fn esp8266_sdk_name(mcu_config: &super::mcu_config::Esp8266McuConfig) -> &str {
    mcu_config
        .defines
        .iter()
        .find_map(|entry| match entry {
            crate::esp32::mcu_config::DefineEntry::KeyValue(name, _)
                if name.starts_with("NONOSDK") =>
            {
                Some(name.as_str())
            }
            _ => None,
        })
        .unwrap_or("NONOSDK22x_190703")
}

/// Create an ESP8266 orchestrator.
pub fn create() -> Box<dyn BuildOrchestrator> {
    Box::new(Esp8266Orchestrator)
}

/// Check if a project is configured for ESP8266 by reading its platformio.ini.
pub fn is_esp8266_project(project_dir: &Path, env_name: &str) -> bool {
    pipeline::is_platform_project(project_dir, env_name, Platform::Espressif8266)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_esp8266_orchestrator_platform() {
        let orch = Esp8266Orchestrator;
        assert_eq!(orch.platform(), Platform::Espressif8266);
    }

    #[test]
    fn test_is_esp8266_project() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:esp8266]\nplatform = espressif8266\nboard = nodemcuv2\nframework = arduino\n",
        )
        .unwrap();
        assert!(is_esp8266_project(tmp.path(), "esp8266"));
        assert!(!is_esp8266_project(tmp.path(), "uno"));
    }

    #[test]
    fn test_is_not_esp8266_project() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:esp32]\nplatform = espressif32\nboard = esp32dev\nframework = arduino\n",
        )
        .unwrap();
        assert!(!is_esp8266_project(tmp.path(), "esp32"));
    }
}
