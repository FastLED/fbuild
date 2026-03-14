//! ESP32 build orchestrator — wires together config, packages, compiler, linker.
//!
//! Build phases:
//! 1. Parse platformio.ini
//! 2. Load board config (esp32dev/esp32c6/etc.)
//! 3. Load MCU config from embedded JSON
//! 4. Ensure ESP32 platform (pioarduino)
//! 5. Resolve + ensure ESP32 toolchain via metadata
//! 6. Ensure ESP32 framework (Arduino core + ESP-IDF SDK libs)
//! 7. Setup build directories
//! 8. Collect include paths: core + variant + SDK (305+) + user src
//! 9. Download + compile library dependencies
//! 10. Scan sources (sketch + core)
//! 11. Compile core sources
//! 12. Compile sketch sources
//! 13. Link (with linker scripts + SDK libs + library archives)
//! 14. Convert to .bin
//! 15. Copy bootloader.bin + partitions.bin
//! 16. Size reporting

use std::path::Path;
use std::time::Instant;

use fbuild_core::{Platform, Result};
use fbuild_packages::Framework;

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
        let mut mcu_config = get_mcu_config(&board.mcu)?;
        tracing::info!(
            "ESP32 build: {} ({}, {})",
            board.name,
            board.mcu,
            mcu_config.architecture
        );

        // 4. Ensure ESP32 platform (pioarduino — contains platform.json with metadata URLs)
        let platform = fbuild_packages::library::Esp32Platform::new(&params.project_dir);
        fbuild_packages::Package::ensure_installed(&platform)?;

        // 5. Resolve + ensure ESP32 toolchain via metadata
        let toolchain = resolve_and_create_toolchain(&platform, &params.project_dir, &mcu_config)?;
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
        let framework = match platform.get_package_url("framework-arduinoespressif32") {
            Ok(url) => {
                tracing::info!("resolved framework URL from platform.json");
                fbuild_packages::library::Esp32Framework::from_url(&params.project_dir, &url)
            }
            Err(e) => {
                tracing::warn!("could not resolve framework URL, using legacy: {}", e);
                fbuild_packages::library::Esp32Framework::new(&params.project_dir, &board.mcu)
            }
        };
        let framework_dir = fbuild_packages::Package::ensure_installed(&framework)?;
        tracing::info!("ESP32 framework at {}", framework_dir.display());

        // 6b. Ensure SDK libs (split package in pioarduino 3.3.7+)
        if let Ok(libs_url) = platform.get_package_url("framework-arduinoespressif32-libs") {
            framework.ensure_libs(&libs_url)?;
        }

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
        // Add SDK include paths (294+ paths from ESP-IDF)
        include_dirs.extend(framework.get_sdk_include_dirs(&board.mcu));

        // Add built-in Arduino library includes (Wire, SPI, WiFi, etc.)
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

        include_dirs.push(src_dir.clone());

        // PlatformIO automatically includes the project's include/ directory
        let include_dir = params.project_dir.join("include");
        if include_dir.is_dir() {
            include_dirs.push(include_dir);
        }

        // Read SDK linker flags early — needed to check LTO before compiling.
        let sdk_ld_flags = framework.get_sdk_ld_flags(&board.mcu);
        let sdk_lib_flags = framework.get_sdk_lib_flags(&board.mcu);
        let sdk_ld_scripts = framework.get_sdk_ld_scripts(&board.mcu);

        // If SDK specifies -fno-lto, disable LTO in MCU config profiles to avoid
        // compiling objects with LTO that the linker can't handle.
        if sdk_ld_flags.iter().any(|f| f == "-fno-lto") {
            mcu_config.disable_lto();
        }

        // 8.5. Library dependencies
        let lib_deps = config.get_lib_deps(&params.env_name)?;
        let lib_ignore = config.get_lib_ignore(&params.env_name)?;

        use fbuild_packages::Toolchain;
        let mut library_archives = Vec::new();

        if !lib_deps.is_empty() {
            let libs_dir = build_dir.join("libs");

            // Build compiler to get flags for library compilation
            let mut defines = board.get_defines();
            defines.extend(mcu_config.defines_map());

            let temp_compiler = Esp32Compiler::new(
                toolchain.get_gcc_path(),
                toolchain.get_gxx_path(),
                mcu_config.clone(),
                &board.f_cpu,
                defines.clone(),
                include_dirs.clone(),
                params.profile,
                params.verbose,
            );

            let c_flags = temp_compiler.c_flags();
            let cpp_flags = temp_compiler.cpp_flags();

            let lib_result = fbuild_packages::library::library_manager::ensure_libraries_sync(
                &lib_deps,
                &lib_ignore,
                &toolchain.get_gcc_path(),
                &toolchain.get_gxx_path(),
                &toolchain.get_ar_path(),
                &c_flags,
                &cpp_flags,
                &include_dirs,
                &libs_dir,
                params.verbose,
            )?;

            // Add library include dirs to the main include list
            include_dirs.extend(lib_result.include_dirs);
            library_archives = lib_result.archives;

            tracing::info!(
                "libraries: {} archives, {} new include dirs",
                library_archives.len(),
                include_dirs.len()
            );
        }

        tracing::info!("include paths: {} total", include_dirs.len());

        // 8.6. Compile framework built-in libraries (WiFi, FS, SPIFFS, Network, etc.)
        // The linker's --gc-sections will strip any unused code.
        {
            let builtin_libs_dir = framework.get_libraries_dir();
            if builtin_libs_dir.is_dir() {
                let fw_libs_build_dir = build_dir.join("fw_libs");
                std::fs::create_dir_all(&fw_libs_build_dir)?;

                // Build set of already-compiled library names
                let already_compiled: std::collections::HashSet<String> = library_archives
                    .iter()
                    .filter_map(|p| p.file_stem())
                    .filter_map(|s| s.to_str())
                    .filter_map(|s| s.strip_prefix("lib"))
                    .map(|s| s.to_string())
                    .collect();

                // Get compiler flags for framework library compilation
                let mut fw_defines = board.get_defines();
                fw_defines.extend(mcu_config.defines_map());

                let fw_compiler = Esp32Compiler::new(
                    toolchain.get_gcc_path(),
                    toolchain.get_gxx_path(),
                    mcu_config.clone(),
                    &board.f_cpu,
                    fw_defines,
                    include_dirs.clone(),
                    params.profile,
                    params.verbose,
                );
                let fw_c_flags = fw_compiler.c_flags();
                let fw_cpp_flags = fw_compiler.cpp_flags();

                let mut fw_lib_count = 0;
                if let Ok(entries) = std::fs::read_dir(&builtin_libs_dir) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if !path.is_dir() {
                            continue;
                        }
                        let lib_name = path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_lowercase();
                        if lib_name.starts_with('.') || already_compiled.contains(&lib_name) {
                            continue;
                        }

                        let lib_src = path.join("src");
                        if !lib_src.is_dir() {
                            continue;
                        }

                        // Check if archive already exists
                        let archive_path = fw_libs_build_dir.join(format!("lib{}.a", lib_name));
                        if archive_path.exists() {
                            library_archives.push(archive_path);
                            fw_lib_count += 1;
                            continue;
                        }

                        // Collect source files
                        let lib_info =
                            fbuild_packages::library::library_info::InstalledLibrary::new(
                                &path, &lib_name,
                            );
                        let sources = lib_info.get_source_files();
                        if sources.is_empty() {
                            continue;
                        }

                        match fbuild_packages::library::library_compiler::compile_library(
                            &lib_name,
                            &sources,
                            &include_dirs,
                            &toolchain.get_gcc_path(),
                            &toolchain.get_gxx_path(),
                            &toolchain.get_ar_path(),
                            &fw_c_flags,
                            &fw_cpp_flags,
                            &fw_libs_build_dir,
                            params.verbose,
                        ) {
                            Ok(Some(archive)) => {
                                library_archives.push(archive);
                                fw_lib_count += 1;
                            }
                            Ok(None) => {} // header-only
                            Err(e) => {
                                // Non-fatal: some framework libs may fail to compile
                                // (e.g., platform-specific ones). The linker will report
                                // if any actually-needed symbols are missing.
                                tracing::debug!(
                                    "framework library {} failed to compile: {}",
                                    lib_name,
                                    e
                                );
                            }
                        }
                    }
                }

                if fw_lib_count > 0 {
                    tracing::info!("compiled {} framework built-in libraries", fw_lib_count);
                }
            }
        }

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
        defines.extend(mcu_config.defines_map());

        // Defines required by the new framework (3.3.7+)
        defines
            .entry("ARDUINO_BOARD".to_string())
            .or_insert_with(|| format!("\"{}\"", board.name));
        defines
            .entry("ARDUINO_VARIANT".to_string())
            .or_insert_with(|| format!("\"{}\"", board.variant));

        let user_flags = config.get_build_flags(&params.env_name)?;

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

        // 11.5. Process embedded files (board_build.embed_files + embed_txtfiles)
        let embed_files = config.get_embed_files(&params.env_name)?;
        let embed_txtfiles = config.get_embed_txtfiles(&params.env_name)?;

        if !embed_files.is_empty() || !embed_txtfiles.is_empty() {
            let embed_dir = build_dir.join("embed");
            std::fs::create_dir_all(&embed_dir)?;

            let objcopy_path = toolchain.get_objcopy_path();
            let (output_target, binary_arch) = if mcu_config.is_riscv() {
                ("elf32-littleriscv", "riscv")
            } else {
                ("elf32-xtensa-le", "xtensa")
            };

            let embed_objects = process_embed_files(
                &embed_files,
                &embed_txtfiles,
                &params.project_dir,
                &embed_dir,
                &objcopy_path,
                output_target,
                binary_arch,
                params.verbose,
            )?;

            sketch_objects.extend(embed_objects);
        }

        // 12-13. Link + convert
        // Library archives join core_objects in the archives parameter
        let mut all_archives: Vec<std::path::PathBuf> = core_objects;
        all_archives.extend(library_archives);

        let linker = Esp32Linker::new(
            toolchain.get_gcc_path(),
            toolchain.get_ar_path(),
            toolchain.get_objcopy_path(),
            toolchain.get_size_path(),
            mcu_config.clone(),
            sdk_ld_flags,
            sdk_lib_flags,
            sdk_ld_scripts,
            params.profile,
            board.max_flash,
            board.max_ram,
            params.verbose,
        );

        let link_result =
            crate::linker::Linker::link_all(&linker, &sketch_objects, &all_archives, &build_dir)?;

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

/// Resolve toolchain URL via platform metadata and create the toolchain instance.
///
/// Falls back to the legacy hardcoded URL constructor if metadata resolution fails.
fn resolve_and_create_toolchain(
    platform: &fbuild_packages::library::Esp32Platform,
    project_dir: &Path,
    mcu_config: &super::mcu_config::Esp32McuConfig,
) -> Result<fbuild_packages::toolchain::Esp32Toolchain> {
    let is_riscv = mcu_config.is_riscv();
    let prefix = mcu_config.toolchain_prefix();

    // Try metadata-based resolution
    match platform.get_toolchain_metadata_url(is_riscv) {
        Ok(metadata_url) => {
            let toolchain_name = if is_riscv {
                "toolchain-riscv32-esp"
            } else {
                "toolchain-xtensa-esp-elf"
            };

            let cache = fbuild_packages::Cache::new(project_dir);
            let cache_dir = cache.toolchains_dir().join(toolchain_name);

            match fbuild_packages::toolchain::esp32_metadata::resolve_toolchain_url_sync(
                &metadata_url,
                toolchain_name,
                &cache_dir,
            ) {
                Ok(resolved) => {
                    tracing::info!("resolved {} toolchain URL from metadata", toolchain_name);
                    Ok(fbuild_packages::toolchain::Esp32Toolchain::from_resolved(
                        project_dir,
                        &resolved.url,
                        resolved.sha256.as_deref(),
                        is_riscv,
                        &prefix,
                    ))
                }
                Err(e) => {
                    tracing::warn!("metadata resolution failed, using legacy URLs: {}", e);
                    Ok(fbuild_packages::toolchain::Esp32Toolchain::new(
                        project_dir,
                        is_riscv,
                        &prefix,
                    ))
                }
            }
        }
        Err(e) => {
            tracing::warn!(
                "could not read platform.json, using legacy toolchain URLs: {}",
                e
            );
            Ok(fbuild_packages::toolchain::Esp32Toolchain::new(
                project_dir,
                is_riscv,
                &prefix,
            ))
        }
    }
}

/// Process `board_build.embed_files` and `board_build.embed_txtfiles`.
///
/// Converts data files into linkable ELF objects using `objcopy --input-target binary`.
/// This generates `_binary_<name>_start`, `_binary_<name>_end`, and `_binary_<name>_size`
/// symbols that the firmware can reference.
///
/// - `embed_files`: embedded as-is (binary)
/// - `embed_txtfiles`: a null-terminated copy is created first, then embedded
#[allow(clippy::too_many_arguments)]
fn process_embed_files(
    embed_files: &[String],
    embed_txtfiles: &[String],
    project_dir: &Path,
    embed_dir: &Path,
    objcopy_path: &Path,
    output_target: &str,
    binary_arch: &str,
    verbose: bool,
) -> Result<Vec<std::path::PathBuf>> {
    use fbuild_core::subprocess::run_command;

    let mut objects = Vec::new();

    // Helper: convert a relative file path to the object file name.
    // e.g. "config/timezones.json" → "config_timezones_json.o"
    let to_obj_name = |path: &str| -> String {
        let sanitized = path.replace(['/', '\\', '.', '-'], "_");
        format!("{}.o", sanitized)
    };

    // Process binary embed files (embed as-is, cwd=project_dir)
    for file in embed_files {
        let src_path = project_dir.join(file);
        if !src_path.exists() {
            tracing::warn!("embed_files: {} not found, skipping", src_path.display());
            continue;
        }

        let obj_name = to_obj_name(file);
        let obj_path = embed_dir.join(&obj_name);

        if obj_path.exists() {
            objects.push(obj_path);
            continue;
        }

        let args = [
            objcopy_path.to_string_lossy().to_string(),
            "--input-target".to_string(),
            "binary".to_string(),
            "--output-target".to_string(),
            output_target.to_string(),
            "--binary-architecture".to_string(),
            binary_arch.to_string(),
            "--rename-section".to_string(),
            ".data=.rodata.embedded".to_string(),
            file.replace('\\', "/"),
            obj_path.to_string_lossy().to_string(),
        ];

        if verbose {
            tracing::info!("embed: {}", args.join(" "));
        }

        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let result = run_command(&args_ref, Some(project_dir), None, None)?;

        if !result.success() {
            return Err(fbuild_core::FbuildError::BuildFailed(format!(
                "objcopy failed for embed file {}:\n{}",
                file, result.stderr
            )));
        }

        tracing::info!("embedded binary file: {}", file);
        objects.push(obj_path);
    }

    // Process text embed files (null-terminated copy, then objcopy from embed_dir)
    for file in embed_txtfiles {
        let src_path = project_dir.join(file);
        if !src_path.exists() {
            tracing::warn!("embed_txtfiles: {} not found, skipping", src_path.display());
            continue;
        }

        let obj_name = to_obj_name(file);
        let obj_path = embed_dir.join(&obj_name);

        if obj_path.exists() {
            objects.push(obj_path);
            continue;
        }

        // Create null-terminated copy in embed_dir preserving relative path
        let rel_dest = embed_dir.join(file);
        if let Some(parent) = rel_dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut content = std::fs::read(&src_path)?;
        if content.last() != Some(&0) {
            content.push(0);
        }
        std::fs::write(&rel_dest, &content)?;

        let args = [
            objcopy_path.to_string_lossy().to_string(),
            "--input-target".to_string(),
            "binary".to_string(),
            "--output-target".to_string(),
            output_target.to_string(),
            "--binary-architecture".to_string(),
            binary_arch.to_string(),
            "--rename-section".to_string(),
            ".data=.rodata.embedded".to_string(),
            file.replace('\\', "/"),
            obj_path.to_string_lossy().to_string(),
        ];

        if verbose {
            tracing::info!("embed txt: {}", args.join(" "));
        }

        // Run from embed_dir so objcopy generates symbols from the relative path
        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let result = run_command(&args_ref, Some(embed_dir), None, None)?;

        if !result.success() {
            return Err(fbuild_core::FbuildError::BuildFailed(format!(
                "objcopy failed for embed txtfile {}:\n{}",
                file, result.stderr
            )));
        }

        tracing::info!("embedded text file: {}", file);
        objects.push(obj_path);
    }

    if !objects.is_empty() {
        tracing::info!("processed {} embedded files", objects.len());
    }

    Ok(objects)
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
