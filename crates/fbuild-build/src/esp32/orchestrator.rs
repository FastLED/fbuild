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

        // 0. Find and start zccache compiler cache (if available)
        let compiler_cache = crate::zccache::find_zccache().map(std::path::Path::to_path_buf);
        if let Some(ref zcc) = compiler_cache {
            crate::zccache::ensure_running(zcc);
        }

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

        let mut build_log = crate::build_output::create_build_log(params.log_sender.clone());
        crate::build_output::log_build_banner(&mut build_log, &params.env_name);
        crate::build_output::log_board_info(
            &mut build_log,
            &board.name,
            &board.mcu,
            &board.f_cpu,
            board.max_flash,
            board.max_ram,
        );

        tracing::info!(
            "ESP32 build: {} ({}, {})",
            board.name,
            board.mcu,
            mcu_config.architecture
        );

        // 4-6. Resolve platform, toolchain, and framework
        let (toolchain, framework) =
            resolve_pioarduino_packages(&params.project_dir, &board.mcu, &mcu_config)?;

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

        // Log toolchain version
        {
            use fbuild_packages::Toolchain as _;
            let tc_label = if mcu_config.is_riscv() {
                "riscv32-esp-elf-gcc"
            } else {
                "xtensa-esp-elf-gcc"
            };
            if let Ok(ver_out) = fbuild_core::subprocess::run_command(
                &[
                    toolchain.get_gcc_path().to_string_lossy().as_ref(),
                    "-dumpversion",
                ],
                None,
                None,
                None,
            ) {
                let version = ver_out.stdout.trim().to_string();
                if !version.is_empty() {
                    crate::build_output::log_toolchain_version(&mut build_log, tc_label, &version);
                }
            }
        }

        let framework_dir = fbuild_packages::Package::ensure_installed(&framework)?;
        tracing::info!("ESP32 framework at {}", framework_dir.display());

        // 7. Setup build directories
        let cache = fbuild_packages::Cache::new(&params.project_dir);
        if params.clean {
            cache.clean_build(&params.env_name, params.profile)?;
        }
        cache.ensure_build_directories(&params.env_name, params.profile)?;

        let build_dir = cache.get_build_dir(&params.env_name, params.profile);
        let core_build_dir = cache.get_core_build_dir(&params.env_name, params.profile);
        let src_build_dir = cache.get_src_build_dir(&params.env_name, params.profile);

        // 8. Collect include paths
        let src_dir = params.project_dir.join(
            config
                .get_src_dir(&params.env_name)?
                .unwrap_or_else(|| "src".to_string()),
        );
        // Fall back to project root if src/ doesn't exist (Arduino IDE convention)
        let src_dir = if src_dir.exists() {
            src_dir
        } else {
            params.project_dir.clone()
        };

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

        // PlatformIO automatically discovers libraries placed in the project's lib/ directory.
        // Each subdirectory is treated as a library — add its root (and src/ if present).
        let local_lib_dir = params.project_dir.join("lib");
        if local_lib_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&local_lib_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        let lib_src = path.join("src");
                        if lib_src.is_dir() {
                            include_dirs.push(lib_src);
                        }
                        // Always add the root too (some libraries have headers at top level)
                        include_dirs.push(path);
                    }
                }
            }
        }

        // Read SDK flags early — needed to check LTO before compiling.
        let sdk_ld_flags = framework.get_sdk_ld_flags(&board.mcu);
        let sdk_lib_flags = framework.get_sdk_lib_flags(&board.mcu);
        let sdk_ld_scripts = framework.get_sdk_ld_scripts(&board.mcu);
        let sdk_defines = framework.get_sdk_defines(&board.mcu);

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

        // Read user build_flags early — needed for both library and sketch compilation.
        // SDK defines (from flags/defines) are prepended so user flags can override them.
        let mut user_flags = sdk_defines;
        let user_build_flags = config.get_build_flags(&params.env_name)?;
        user_flags.extend(user_build_flags.clone());

        // Emit a warning if CDC on boot is effectively enabled (may cause Serial to block
        // when no USB host is connected).
        warn_if_cdc_on_boot(&board.name, board.extra_flags.as_deref(), &user_build_flags);
        crate::warn_debug_build_flags(&user_build_flags);

        if !lib_deps.is_empty() {
            let libs_dir = build_dir.join("libs");

            // Build compiler to get flags for library compilation
            let mut defines = board.get_defines();
            defines.extend(mcu_config.defines_map());

            let temp_compiler = Esp32Compiler::with_temp_dir(
                toolchain.get_gcc_path(),
                toolchain.get_gxx_path(),
                mcu_config.clone(),
                &board.f_cpu,
                defines.clone(),
                include_dirs.clone(),
                params.profile,
                params.verbose,
                build_dir.join("tmp"),
            );
            // Apply user build_flags to library compilation (matching PlatformIO behavior).
            // User flags like -std=gnu++2a replace the MCU config's -std=gnu++2b.
            let c_flags = apply_user_flags(&temp_compiler.c_flags(), &user_flags);
            let cpp_flags = apply_user_flags(&temp_compiler.cpp_flags(), &user_flags);

            let jobs = crate::parallel::effective_jobs(params.jobs);
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
                jobs,
                compiler_cache.as_deref(),
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
        // Skip when only generating compile_commands.json.
        if !params.compiledb_only {
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

                let fw_compiler = Esp32Compiler::with_temp_dir(
                    toolchain.get_gcc_path(),
                    toolchain.get_gxx_path(),
                    mcu_config.clone(),
                    &board.f_cpu,
                    fw_defines,
                    include_dirs.clone(),
                    params.profile,
                    params.verbose,
                    build_dir.join("tmp"),
                );
                let fw_c_flags = apply_user_flags(&fw_compiler.c_flags(), &user_flags);
                let fw_cpp_flags = apply_user_flags(&fw_compiler.cpp_flags(), &user_flags);

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

                        let fw_jobs = crate::parallel::effective_jobs(params.jobs);
                        match fbuild_packages::library::library_compiler::compile_library_with_jobs(
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
                            fw_jobs,
                            compiler_cache.as_deref(),
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

        // Defines required by the new framework (3.3.7+).
        // Use \" escapes for GCC response file compatibility (see board.rs).
        defines
            .entry("ARDUINO_BOARD".to_string())
            .or_insert_with(|| format!("\\\"{}\\\"", board.name));
        defines
            .entry("ARDUINO_VARIANT".to_string())
            .or_insert_with(|| format!("\\\"{}\\\"", board.variant));

        let compiler = Esp32Compiler::with_temp_dir(
            toolchain.get_gcc_path(),
            toolchain.get_gxx_path(),
            mcu_config.clone(),
            &board.f_cpu,
            defines,
            include_dirs.clone(),
            params.profile,
            params.verbose,
            build_dir.join("tmp"),
        );
        let jobs = crate::parallel::effective_jobs(params.jobs);
        tracing::info!("parallel compilation: {} jobs", jobs);

        // Build source lists and flags needed for compile_commands.json
        let mut all_core_sources: Vec<std::path::PathBuf> = Vec::new();
        all_core_sources.extend(sources.core_sources.iter().cloned());
        all_core_sources.extend(sources.variant_sources.iter().cloned());

        let src_flags = config.get_build_src_flags(&params.env_name)?;
        let all_src_flags: Vec<String> =
            user_flags.iter().chain(src_flags.iter()).cloned().collect();

        // compiledb_only: generate compile_commands.json without compiling
        if params.compiledb_only {
            let mut compile_db = crate::compile_database::CompileDatabase::new();
            let include_flags = compiler.base.build_include_flags();
            compile_db.extend(crate::compile_database::generate_entries(
                compiler.gcc_path(),
                compiler.gxx_path(),
                &compiler.c_flags(),
                &compiler.cpp_flags(),
                &include_flags,
                &user_flags,
                &all_core_sources,
                &core_build_dir,
                &params.project_dir,
            ));
            compile_db.extend(crate::compile_database::generate_entries(
                compiler.gcc_path(),
                compiler.gxx_path(),
                &compiler.c_flags(),
                &compiler.cpp_flags(),
                &include_flags,
                &all_src_flags,
                &sources.sketch_sources,
                &src_build_dir,
                &params.project_dir,
            ));
            let arch = if mcu_config.is_xtensa() {
                crate::compile_database::TargetArchitecture::Xtensa
            } else {
                crate::compile_database::TargetArchitecture::Riscv32
            };
            let compile_db = compile_db.translate_for_clang(arch);
            let compile_database_path = if compile_db.has_entries() {
                Some(compile_db.write_and_copy(&build_dir, &params.project_dir)?)
            } else {
                None
            };
            let elapsed = start.elapsed().as_secs_f64();
            return Ok(BuildResult {
                success: true,
                firmware_path: None,
                elf_path: None,
                size_info: None,
                build_time_secs: elapsed,
                message: format!(
                    "compile_commands.json generated for {} ({})",
                    params.env_name, board.mcu
                ),
                compile_database_path,
                build_log,
            });
        }

        // Compile core + variant sources in parallel
        let build_log_mutex = std::sync::Mutex::new(build_log);
        let core_result = crate::parallel::compile_sources_parallel(
            &compiler,
            &all_core_sources,
            &core_build_dir,
            &user_flags,
            jobs,
            Some(&build_log_mutex),
        )?;

        // Compile sketch sources in parallel
        let sketch_result = crate::parallel::compile_sources_parallel(
            &compiler,
            &sources.sketch_sources,
            &src_build_dir,
            &all_src_flags,
            jobs,
            Some(&build_log_mutex),
        )?;

        // Unwrap build log and flush collected warnings
        let mut build_log = build_log_mutex.into_inner().unwrap();
        for w in core_result.warnings.iter().chain(&sketch_result.warnings) {
            crate::build_output::collect_warnings(w, &mut build_log);
        }

        let core_objects = core_result.objects;
        let mut sketch_objects = sketch_result.objects;

        // Compile local libraries from the project's lib/ directory.
        // PlatformIO discovers and compiles these automatically.
        let local_lib_dir = params.project_dir.join("lib");
        if local_lib_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&local_lib_dir) {
                for entry in entries.flatten() {
                    let lib_path = entry.path();
                    if !lib_path.is_dir() {
                        continue;
                    }
                    let lib_name = lib_path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();

                    let lib_info = fbuild_packages::library::library_info::InstalledLibrary::new(
                        &lib_path, &lib_name,
                    );
                    let lib_sources = lib_info.get_source_files();
                    if lib_sources.is_empty() {
                        continue;
                    }

                    let lib_build_dir = build_dir.join("lib").join(&lib_name);
                    tracing::info!(
                        "compiling local library '{}': {} source files",
                        lib_name,
                        lib_sources.len()
                    );

                    match fbuild_packages::library::library_compiler::compile_library_with_jobs(
                        &lib_name,
                        &lib_sources,
                        &include_dirs,
                        &toolchain.get_gcc_path(),
                        &toolchain.get_gxx_path(),
                        &toolchain.get_ar_path(),
                        &apply_user_flags(&compiler.c_flags(), &all_src_flags),
                        &apply_user_flags(&compiler.cpp_flags(), &all_src_flags),
                        &lib_build_dir,
                        params.verbose,
                        jobs,
                        compiler_cache.as_deref(),
                    ) {
                        Ok(Some(archive)) => {
                            library_archives.push(archive);
                        }
                        Ok(None) => {} // header-only
                        Err(e) => {
                            return Err(fbuild_core::FbuildError::BuildFailed(format!(
                                "local library '{}' failed to compile: {}",
                                lib_name, e
                            )));
                        }
                    }
                }
            }
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

        // 11.6. Generate compile_commands.json
        let mut compile_db = crate::compile_database::CompileDatabase::new();
        let include_flags = compiler.base.build_include_flags();
        // Core + variant sources use user_flags
        compile_db.extend(crate::compile_database::generate_entries(
            compiler.gcc_path(),
            compiler.gxx_path(),
            &compiler.c_flags(),
            &compiler.cpp_flags(),
            &include_flags, // ESP32: include flags separate from c/cpp_flags
            &user_flags,
            &all_core_sources,
            &core_build_dir,
            &params.project_dir,
        ));
        // Sketch sources use all_src_flags
        compile_db.extend(crate::compile_database::generate_entries(
            compiler.gcc_path(),
            compiler.gxx_path(),
            &compiler.c_flags(),
            &compiler.cpp_flags(),
            &include_flags,
            &all_src_flags,
            &sources.sketch_sources,
            &src_build_dir,
            &params.project_dir,
        ));
        let arch = if mcu_config.is_xtensa() {
            crate::compile_database::TargetArchitecture::Xtensa
        } else {
            crate::compile_database::TargetArchitecture::Riscv32
        };
        let compile_db = compile_db.translate_for_clang(arch);
        let compile_database_path = if compile_db.has_entries() {
            Some(compile_db.write_and_copy(&build_dir, &params.project_dir)?)
        } else {
            None
        };

        // 12-13. Link + convert
        // Library archives join core_objects in the archives parameter
        let mut all_archives: Vec<std::path::PathBuf> = core_objects;
        all_archives.extend(library_archives);

        let flash_freq = crate::esp32::esp32_linker::f_flash_to_esptool_freq(
            board.f_flash.as_deref(),
            mcu_config.default_flash_freq(),
        );
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
            board.flash_mode.clone(),
            flash_freq,
            board.max_flash,
            board.max_ram,
            params.verbose,
        );

        let link_result =
            crate::linker::Linker::link_all(&linker, &sketch_objects, &all_archives, &build_dir)?;

        // 14. Prepare bootloader.bin + partitions.bin for deployment
        let boot_dst = build_dir.join("bootloader.bin");
        let boot_bin_src = framework.get_bootloader_bin(&board.mcu);
        if boot_bin_src.exists() {
            // Pre-built bootloader.bin available — just copy
            std::fs::copy(&boot_bin_src, &boot_dst)?;
            tracing::info!("copied bootloader.bin");
        } else {
            // Convert bootloader ELF to BIN using esptool elf2image.
            // The bootloader ELF filename encodes the flash mode and frequency.
            // ESP32 ROM bootloader typically requires DIO mode regardless of
            // application flash mode, but we use the board's configured mode
            // since the Arduino core names the ELF accordingly.
            let boot_flash_mode = board
                .flash_mode
                .as_deref()
                .unwrap_or(mcu_config.default_flash_mode());
            let boot_elf = framework.get_bootloader_elf(&board.mcu, boot_flash_mode, flash_freq);
            if boot_elf.exists() {
                let boot_elf_str = boot_elf.to_string_lossy();
                let boot_dst_str = boot_dst.to_string_lossy();
                let flash_size = crate::esp32::mcu_config::bytes_to_flash_size(
                    board.max_flash,
                    mcu_config.default_flash_size(),
                );
                let args = [
                    "esptool",
                    "--chip",
                    &board.mcu,
                    "elf2image",
                    "--flash-mode",
                    boot_flash_mode,
                    "--flash-freq",
                    flash_freq,
                    "--flash-size",
                    flash_size,
                    &boot_elf_str,
                    "-o",
                    &boot_dst_str,
                ];
                match fbuild_core::subprocess::run_command(
                    &args,
                    None,
                    None,
                    Some(std::time::Duration::from_secs(30)),
                ) {
                    Ok(result) if result.success() => {
                        tracing::info!("converted bootloader ELF → bootloader.bin");
                    }
                    Ok(result) => {
                        tracing::warn!(
                            "bootloader elf2image failed: {}{}",
                            result.stderr,
                            result.stdout
                        );
                    }
                    Err(e) => {
                        tracing::warn!("esptool not found for bootloader conversion: {}", e);
                    }
                }
            } else {
                tracing::warn!(
                    "no bootloader found at {} or {}",
                    boot_bin_src.display(),
                    boot_elf.display()
                );
            }
        }

        let parts_dst = build_dir.join("partitions.bin");
        let parts_bin_src = framework.get_partitions_bin(&board.mcu);
        if parts_bin_src.exists() {
            // Pre-built partitions.bin available — just copy
            std::fs::copy(&parts_bin_src, &parts_dst)?;
            tracing::info!("copied partitions.bin");
        } else {
            // Generate partitions.bin from CSV using gen_esp32part.py
            let partitions_name = board.partitions.as_deref().unwrap_or("default.csv");
            let parts_csv = framework.get_partitions_csv(partitions_name);
            let gen_tool = framework.get_gen_esp32part();
            if parts_csv.exists() && gen_tool.exists() {
                let gen_tool_str = gen_tool.to_string_lossy();
                let parts_csv_str = parts_csv.to_string_lossy();
                let parts_dst_str = parts_dst.to_string_lossy();
                let args = [
                    "python",
                    &gen_tool_str,
                    "-q",
                    &parts_csv_str,
                    &parts_dst_str,
                ];
                match fbuild_core::subprocess::run_command(
                    &args,
                    None,
                    None,
                    Some(std::time::Duration::from_secs(10)),
                ) {
                    Ok(result) if result.success() => {
                        tracing::info!("generated partitions.bin from {}", partitions_name);
                    }
                    Ok(result) => {
                        tracing::warn!("gen_esp32part.py failed: {}", result.stderr);
                    }
                    Err(e) => {
                        tracing::warn!("python not found for partitions generation: {}", e);
                    }
                }
            } else {
                tracing::warn!(
                    "no partitions source: csv={} gen_tool={}",
                    parts_csv.display(),
                    gen_tool.display()
                );
            }
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
            crate::build_output::log_size_report(&mut build_log, size);
        }

        // Artifact listing
        if let Some(ref elf) = link_result.elf_path {
            crate::build_output::log_artifact(&mut build_log, elf);
        }
        if let Some(fw) = link_result
            .bin_path
            .as_ref()
            .or(link_result.hex_path.as_ref())
        {
            crate::build_output::log_artifact(&mut build_log, fw);
        }

        let elapsed = start.elapsed().as_secs_f64();
        tracing::info!("build completed in {:.1}s", elapsed);

        Ok(BuildResult {
            success: true,
            firmware_path: link_result.bin_path.clone().or(link_result.hex_path),
            elf_path: link_result.elf_path,
            size_info: link_result.size_info,
            build_time_secs: elapsed,
            message: format!(
                "ESP32 build for {} ({}) completed",
                params.env_name, board.mcu
            ),
            compile_database_path,
            build_log,
        })
    }
}

/// Apply user build_flags from platformio.ini onto base compiler flags.
///
/// Matches PlatformIO behavior: user flags are appended to common flags,
/// but `-std=` flags replace the existing standard (not stack).
fn apply_user_flags(base_flags: &[String], user_flags: &[String]) -> Vec<String> {
    let mut result = base_flags.to_vec();
    for flag in user_flags {
        if flag.starts_with("-std=") {
            // Replace any existing -std= flag
            result.retain(|f| !f.starts_with("-std="));
        }
        result.push(flag.clone());
    }
    result
}

/// Resolve framework + toolchain for pioarduino mode (GCC 14 + ESP-IDF 5.x).
///
/// Downloads pioarduino platform.json, resolves toolchain via metadata,
/// and downloads the split framework + libs packages.
fn resolve_pioarduino_packages(
    project_dir: &Path,
    mcu: &str,
    mcu_config: &super::mcu_config::Esp32McuConfig,
) -> Result<(
    fbuild_packages::toolchain::Esp32Toolchain,
    fbuild_packages::library::Esp32Framework,
)> {
    // Ensure pioarduino platform (contains platform.json with metadata URLs)
    let platform = fbuild_packages::library::Esp32Platform::new(project_dir);
    fbuild_packages::Package::ensure_installed(&platform)?;

    // Resolve toolchain via metadata
    let toolchain = resolve_and_create_toolchain(&platform, project_dir, mcu_config)?;

    // Resolve framework
    let framework = match platform.get_package_url("framework-arduinoespressif32") {
        Ok(url) => {
            tracing::info!("resolved framework URL from platform.json");
            fbuild_packages::library::Esp32Framework::from_url(project_dir, &url)
        }
        Err(e) => {
            tracing::warn!("could not resolve framework URL, using legacy: {}", e);
            fbuild_packages::library::Esp32Framework::new(project_dir, mcu)
        }
    };

    // Ensure framework is installed before trying to install libs
    let _ = fbuild_packages::Package::ensure_installed(&framework)?;

    // Ensure SDK libs (split package in pioarduino 3.3.7+)
    if let Ok(libs_url) = platform.get_package_url("framework-arduinoespressif32-libs") {
        framework.ensure_libs(&libs_url)?;
    }

    Ok((toolchain, framework))
}

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

/// Determine whether ARDUINO_USB_CDC_ON_BOOT is effectively enabled.
///
/// Combines `board_extra_flags` (a space-separated string from the board JSON) with
/// `user_build_flags` (from platformio.ini `build_flags`).  Board flags are applied
/// first; user flags can override them.  The **last** definition of
/// `-DARDUINO_USB_CDC_ON_BOOT=N` wins, matching C preprocessor semantics.
///
/// Returns `true` only if the final effective value is `1`.
pub fn cdc_on_boot_enabled(board_extra_flags: Option<&str>, user_build_flags: &[String]) -> bool {
    // Collect all flags in application order: board first, then user.
    let board_tokens: Vec<String> = board_extra_flags
        .unwrap_or("")
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();

    let all_flags: Vec<&str> = board_tokens
        .iter()
        .map(|s| s.as_str())
        .chain(user_build_flags.iter().map(|s| s.as_str()))
        .collect();

    let mut effective: Option<bool> = None;

    for flag in &all_flags {
        // Normalise: strip leading whitespace and optional `-D` prefix added by some tools.
        let stripped = flag.trim();
        // Match `-DARDUINO_USB_CDC_ON_BOOT=VALUE` or `ARDUINO_USB_CDC_ON_BOOT=VALUE`
        let without_d = stripped.strip_prefix("-D").unwrap_or(stripped);

        if let Some(value) = without_d.strip_prefix("ARDUINO_USB_CDC_ON_BOOT=") {
            effective = Some(value.trim() == "1");
        }
    }

    effective.unwrap_or(false)
}

/// Emit a `tracing::warn!` if CDC on boot is effectively enabled.
///
/// `ARDUINO_USB_CDC_ON_BOOT=1` initialises the USB CDC port during boot via native
/// USB (ESP32-S3, C3, C6, S2, …).  When no USB host is connected at power-on any
/// call to `Serial.print()` will block indefinitely because the CDC TX buffer has no
/// consumer to drain it.
pub fn warn_if_cdc_on_boot(
    board_name: &str,
    board_extra_flags: Option<&str>,
    user_build_flags: &[String],
) {
    if cdc_on_boot_enabled(board_extra_flags, user_build_flags) {
        tracing::warn!(
            "Board '{}' has ARDUINO_USB_CDC_ON_BOOT=1.  \
             If no USB host is connected at power-on, Serial.print() will block \
             indefinitely.  Add -DARDUINO_USB_CDC_ON_BOOT=0 to build_flags to suppress this warning.",
            board_name
        );
    }
}

/// Check if a project is configured for ESP32 by reading its platformio.ini.
pub fn is_esp32_project(project_dir: &Path, env_name: &str) -> bool {
    let ini_path = project_dir.join("platformio.ini");
    if let Ok(config) = fbuild_config::PlatformIOConfig::from_path(&ini_path) {
        if let Ok(env) = config.get_env_config(env_name) {
            if let Some(platform) = env.get("platform") {
                return fbuild_core::Platform::Espressif32.matches_str(platform);
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

    // --- CDC on boot warning tests ---

    /// Board that enables CDC on boot via extra_flags (e.g. Adafruit Feather ESP32-S3).
    #[test]
    fn test_cdc_enabled_by_board_extra_flags() {
        let board_flags = Some(
            "-DARDUINO_ADAFRUIT_FEATHER_ESP32S3 -DARDUINO_USB_CDC_ON_BOOT=1 -DARDUINO_RUNNING_CORE=1"
        );
        assert!(cdc_on_boot_enabled(board_flags, &[]));
    }

    /// Board that explicitly disables CDC on boot.
    #[test]
    fn test_cdc_disabled_by_board_extra_flags() {
        let board_flags = Some("-DARDUINO_FREENOVE_ESP32_S3_WROOM -DARDUINO_USB_CDC_ON_BOOT=0");
        assert!(!cdc_on_boot_enabled(board_flags, &[]));
    }

    /// Plain ESP32 dev board with no CDC flag at all — not enabled.
    #[test]
    fn test_no_cdc_flag_returns_false() {
        let board_flags = Some("-DARDUINO_ESP32_DEV");
        assert!(!cdc_on_boot_enabled(board_flags, &[]));
    }

    /// No board flags at all — not enabled.
    #[test]
    fn test_no_flags_at_all_returns_false() {
        assert!(!cdc_on_boot_enabled(None, &[]));
    }

    /// User build_flags override a board-level enable (last definition wins).
    #[test]
    fn test_user_flag_overrides_board_enable() {
        let board_flags = Some("-DARDUINO_USB_CDC_ON_BOOT=1");
        let user_flags = vec!["-DARDUINO_USB_CDC_ON_BOOT=0".to_string()];
        assert!(!cdc_on_boot_enabled(board_flags, &user_flags));
    }

    /// User build_flags can enable CDC that the board left unconfigured.
    #[test]
    fn test_user_flag_enables_cdc() {
        let board_flags = Some("-DARDUINO_ESP32_DEV");
        let user_flags = vec!["-DARDUINO_USB_CDC_ON_BOOT=1".to_string()];
        assert!(cdc_on_boot_enabled(board_flags, &user_flags));
    }

    /// Multiple user flags — last one wins.
    #[test]
    fn test_last_user_flag_wins() {
        let board_flags = Some("-DARDUINO_USB_CDC_ON_BOOT=1");
        let user_flags = vec![
            "-DARDUINO_USB_CDC_ON_BOOT=0".to_string(),
            "-DARDUINO_USB_CDC_ON_BOOT=1".to_string(),
        ];
        assert!(cdc_on_boot_enabled(board_flags, &user_flags));
    }

    /// Flags provided as whitespace-separated string should be parsed correctly.
    #[test]
    fn test_multi_flag_string_parsed_correctly() {
        // Board flags: the enable flag appears after another flag.
        let board_flags = Some("-DSOME_DEFINE -DARDUINO_USB_CDC_ON_BOOT=1 -DANOTHER=1");
        assert!(cdc_on_boot_enabled(board_flags, &[]));
    }

    /// `warn_if_cdc_on_boot` should not panic for any combination of inputs.
    #[test]
    fn test_warn_if_cdc_on_boot_no_panic() {
        // CDC enabled — triggers warning path
        warn_if_cdc_on_boot(
            "Adafruit Feather ESP32-S3",
            Some("-DARDUINO_USB_CDC_ON_BOOT=1"),
            &[],
        );
        // CDC disabled — no warning
        warn_if_cdc_on_boot(
            "Freenove ESP32-S3-WROOM",
            Some("-DARDUINO_USB_CDC_ON_BOOT=0"),
            &[],
        );
        // No flag at all — no warning
        warn_if_cdc_on_boot("ESP32 Dev Module", Some("-DARDUINO_ESP32_DEV"), &[]);
        // No board flags — no warning
        warn_if_cdc_on_boot("Some Board", None, &[]);
        // User override suppresses board enable
        warn_if_cdc_on_boot(
            "Some Board",
            Some("-DARDUINO_USB_CDC_ON_BOOT=1"),
            &["-DARDUINO_USB_CDC_ON_BOOT=0".to_string()],
        );
    }
}
