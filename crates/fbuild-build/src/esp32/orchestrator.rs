//! ESP32 build orchestrator — wires together config, packages, compiler, linker.
//!
//! Build phases:
//! 1. Parse platformio.ini
//! 2. Load board config (esp32dev/esp32c6/etc.)
//! 3. Load MCU config from embedded JSON
//! 4. Determine toolchain type (RISC-V vs Xtensa)
//! 5. Ensure ESP32 toolchain
//! 6. Ensure ESP32 framework (Arduino core + ESP-IDF SDK libs)
//! 7. Setup build directories
//! 8. Collect include paths: core + variant + SDK (305+) + user src
//! 9. Scan sources (sketch + core)
//! 10. Compile core sources
//! 11. Compile sketch sources
//! 12. Link (with linker scripts + SDK libs)
//! 13. Convert to .bin
//! 14. Copy bootloader.bin + partitions.bin
//! 15. Size reporting

use std::path::Path;
use std::time::Instant;

use fbuild_core::{Platform, Result};

use crate::compiler::{Compiler, CompilerBase};
use crate::{BuildOrchestrator, BuildParams, BuildResult, SourceScanner};

use super::esp32_compiler::Esp32Compiler;
use super::esp32_linker::Esp32Linker;
use super::mcu_config::get_mcu_config;

/// ESP32 platform build orchestrator.
pub struct Esp32Orchestrator;

impl BuildOrchestrator for Esp32Orchestrator {
    fn platform(&self) -> Platform {
        Platform::Espressif32
    }

    fn build(&self, params: &BuildParams) -> Result<BuildResult> {
        let start = Instant::now();

        // 1. Parse platformio.ini
        let ini_path = params.project_dir.join("platformio.ini");
        let config = fbuild_config::PlatformIOConfig::from_path(&ini_path)?;
        let env_config = config.get_env_config(&params.env_name)?;

        // 2. Load board config
        let board_id = env_config.get("board").ok_or_else(|| {
            fbuild_core::FbuildError::ConfigError("missing 'board' in environment config".into())
        })?;
        let overrides = config.get_board_overrides(&params.env_name)?;
        let board = fbuild_config::BoardConfig::from_board_id(board_id, &overrides)?;

        // 3. Load MCU config from embedded JSON
        let mcu_config = get_mcu_config(&board.mcu)?;
        tracing::info!(
            "ESP32 build: {} ({}, {})",
            board.name,
            board.mcu,
            mcu_config.architecture
        );

        // 4-5. Ensure ESP32 toolchain (RISC-V or Xtensa based on architecture)
        let toolchain = fbuild_packages::toolchain::Esp32Toolchain::new(
            &params.project_dir,
            mcu_config.is_riscv(),
            mcu_config.toolchain_prefix(),
        );
        let toolchain_dir = fbuild_packages::Package::ensure_installed(&toolchain)?;
        tracing::info!(
            "ESP32 {} toolchain at {}",
            if mcu_config.is_riscv() {
                "RISC-V"
            } else {
                "Xtensa"
            },
            toolchain_dir.display()
        );

        // 6. Ensure ESP32 framework (Arduino core + ESP-IDF SDK)
        let framework =
            fbuild_packages::library::Esp32Framework::new(&params.project_dir, &board.mcu);
        let framework_dir = fbuild_packages::Package::ensure_installed(&framework)?;
        tracing::info!("ESP32 framework at {}", framework_dir.display());

        // 7. Setup build directories
        let cache = fbuild_packages::Cache::new(&params.project_dir);
        if params.clean {
            cache.clean_build(&params.env_name)?;
        }
        cache.ensure_build_directories(&params.env_name)?;

        let build_dir = cache.get_build_dir(&params.env_name);
        let core_build_dir = cache.get_core_build_dir(&params.env_name);
        let src_build_dir = cache.get_src_build_dir(&params.env_name);

        // 8. Collect include paths
        let src_dir = params.project_dir.join(
            config
                .get_src_dir(&params.env_name)?
                .unwrap_or_else(|| "src".to_string()),
        );

        let core_dir = framework.get_core_dir(&board.core);
        let variant_dir = framework.get_variant_dir(&board.variant);

        let mut include_dirs = vec![core_dir.clone()];
        if variant_dir.exists() {
            include_dirs.push(variant_dir.clone());
        }
        // Add SDK include paths (305+ paths from ESP-IDF)
        include_dirs.extend(framework.get_sdk_include_dirs(&board.mcu));
        include_dirs.push(src_dir.clone());

        tracing::info!("include paths: {} total", include_dirs.len());

        // 9. Scan sources
        let scanner = SourceScanner::new(&src_dir, &src_build_dir);
        let variant_dir_opt = if variant_dir.exists() {
            Some(variant_dir.as_path())
        } else {
            None
        };
        let sources = scanner.scan_all(Some(&core_dir), variant_dir_opt)?;

        tracing::info!(
            "sources: {} sketch, {} core, {} variant",
            sources.sketch_sources.len(),
            sources.core_sources.len(),
            sources.variant_sources.len(),
        );

        // 10-11. Compile
        let mut defines = board.get_defines();
        // Merge MCU config defines
        defines.extend(mcu_config.defines_map());

        let user_flags = config.get_build_flags(&params.env_name)?;

        use fbuild_packages::Toolchain;
        let compiler = Esp32Compiler::new(
            toolchain.get_gcc_path(),
            toolchain.get_gxx_path(),
            mcu_config.clone(),
            &board.f_cpu,
            defines,
            include_dirs,
            params.profile,
            params.verbose,
        );

        // Compile core sources
        let mut core_objects = Vec::new();
        for source in &sources.core_sources {
            let obj = CompilerBase::object_path(source, &core_build_dir);
            if CompilerBase::needs_rebuild(source, &obj) {
                let result = compiler.compile(source, &obj, &user_flags)?;
                if !result.success {
                    return Err(fbuild_core::FbuildError::BuildFailed(format!(
                        "compilation failed for {}:\n{}",
                        source.display(),
                        result.stderr
                    )));
                }
            }
            core_objects.push(obj);
        }

        // Compile variant sources
        for source in &sources.variant_sources {
            let obj = CompilerBase::object_path(source, &core_build_dir);
            if CompilerBase::needs_rebuild(source, &obj) {
                let result = compiler.compile(source, &obj, &user_flags)?;
                if !result.success {
                    return Err(fbuild_core::FbuildError::BuildFailed(format!(
                        "compilation failed for {}:\n{}",
                        source.display(),
                        result.stderr
                    )));
                }
            }
            core_objects.push(obj);
        }

        // Compile sketch sources
        let src_flags = config.get_build_src_flags(&params.env_name)?;
        let all_src_flags: Vec<String> =
            user_flags.iter().chain(src_flags.iter()).cloned().collect();

        let mut sketch_objects = Vec::new();
        for source in &sources.sketch_sources {
            let obj = CompilerBase::object_path(source, &src_build_dir);
            if CompilerBase::needs_rebuild(source, &obj) {
                let result = compiler.compile(source, &obj, &all_src_flags)?;
                if !result.success {
                    return Err(fbuild_core::FbuildError::BuildFailed(format!(
                        "compilation failed for {}:\n{}",
                        source.display(),
                        result.stderr
                    )));
                }
            }
            sketch_objects.push(obj);
        }

        // 12-13. Link + convert
        let linker_scripts_dir = framework.get_linker_scripts_dir(&board.mcu);
        let sdk_libs = framework.get_sdk_libs(&board.mcu);

        let linker = Esp32Linker::new(
            toolchain.get_gcc_path(),
            toolchain.get_ar_path(),
            toolchain.get_objcopy_path(),
            toolchain.get_size_path(),
            mcu_config.clone(),
            linker_scripts_dir,
            sdk_libs,
            params.profile,
            board.max_flash,
            board.max_ram,
            params.verbose,
        );

        let link_result =
            crate::linker::Linker::link_all(&linker, &sketch_objects, &core_objects, &build_dir)?;

        // 14. Copy bootloader.bin + partitions.bin
        let boot_src = framework.get_bootloader_bin(&board.mcu);
        let parts_src = framework.get_partitions_bin(&board.mcu);
        if boot_src.exists() {
            let boot_dst = build_dir.join("bootloader.bin");
            std::fs::copy(&boot_src, &boot_dst)?;
            tracing::info!("copied bootloader.bin");
        }
        if parts_src.exists() {
            let parts_dst = build_dir.join("partitions.bin");
            std::fs::copy(&parts_src, &parts_dst)?;
            tracing::info!("copied partitions.bin");
        }

        // 15. Size reporting
        if let Some(ref size) = link_result.size_info {
            tracing::info!(
                "size: text={} data={} bss={} | flash={}/{} ({:.1}%) ram={}/{} ({:.1}%)",
                size.text,
                size.data,
                size.bss,
                size.total_flash,
                size.max_flash.unwrap_or(0),
                size.flash_percent().unwrap_or(0.0),
                size.total_ram,
                size.max_ram.unwrap_or(0),
                size.ram_percent().unwrap_or(0.0),
            );
        }

        let elapsed = start.elapsed().as_secs_f64();
        tracing::info!("build completed in {:.1}s", elapsed);

        Ok(BuildResult {
            success: true,
            hex_path: link_result.bin_path.clone().or(link_result.hex_path),
            elf_path: link_result.elf_path,
            size_info: link_result.size_info,
            build_time_secs: elapsed,
            message: format!(
                "ESP32 build for {} ({}) completed",
                params.env_name, board.mcu
            ),
        })
    }
}

/// Create an ESP32 orchestrator (convenience for get_orchestrator dispatch).
pub fn create() -> Box<dyn BuildOrchestrator> {
    Box::new(Esp32Orchestrator)
}

/// Check if a project is configured for ESP32 by reading its platformio.ini.
pub fn is_esp32_project(project_dir: &Path, env_name: &str) -> bool {
    let ini_path = project_dir.join("platformio.ini");
    if let Ok(config) = fbuild_config::PlatformIOConfig::from_path(&ini_path) {
        if let Ok(env) = config.get_env_config(env_name) {
            if let Some(platform) = env.get("platform") {
                return platform == "espressif32";
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_esp32_orchestrator_platform() {
        let orch = Esp32Orchestrator;
        assert_eq!(orch.platform(), Platform::Espressif32);
    }

    #[test]
    fn test_is_esp32_project() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:esp32c6]\nplatform = espressif32\nboard = esp32-c6\nframework = arduino\n",
        )
        .unwrap();
        assert!(is_esp32_project(tmp.path(), "esp32c6"));
        assert!(!is_esp32_project(tmp.path(), "uno"));
    }

    #[test]
    fn test_is_not_esp32_project() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:uno]\nplatform = atmelavr\nboard = uno\nframework = arduino\n",
        )
        .unwrap();
        assert!(!is_esp32_project(tmp.path(), "uno"));
    }
}
