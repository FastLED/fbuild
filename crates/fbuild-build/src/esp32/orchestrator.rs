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

use std::path::{Path, PathBuf};
use std::time::Instant;

use fbuild_core::{Platform, Result};
use fbuild_packages::Framework;
use serde::Serialize;

use crate::build_fingerprint::{
    hash_watch_set_stamps_cached, save_json, stable_hash_json, PersistedBuildFingerprint,
    BUILD_FINGERPRINT_VERSION,
};
use crate::flag_overlay::LanguageExtraFlags;
use crate::linker::LinkerScripts;
use crate::zccache::FingerprintWatch;

use crate::compiler::Compiler as _;
use crate::{BuildOrchestrator, BuildParams, BuildResult, SourceScanner};

use super::esp32_compiler::Esp32Compiler;
use super::esp32_linker::Esp32Linker;
use super::mcu_config::get_mcu_config;

/// ESP32 platform build orchestrator.
pub struct Esp32Orchestrator;

#[derive(Debug, Serialize)]
struct Esp32FingerprintMetadata {
    version: u32,
    env_name: String,
    profile: String,
    board_name: String,
    board_mcu: String,
    board_define: String,
    board_core: String,
    board_variant: String,
    board_variant_h: Option<String>,
    board_extra_flags: Option<String>,
    board_upload_protocol: Option<String>,
    board_upload_speed: Option<String>,
    board_partitions: Option<String>,
    board_ldscript: Option<String>,
    board_platform: Option<String>,
    architecture: String,
    platform: String,
    flash_mode: String,
    flash_freq: String,
    flash_size: String,
    max_flash: Option<u64>,
    max_ram: Option<u64>,
}

fn framework_failure_marker(build_dir: &Path, lib_name: &str) -> PathBuf {
    build_dir.join(format!(".{lib_name}.failed"))
}

fn framework_signature(
    include_dirs: &[PathBuf],
    c_flags: &[String],
    cpp_flags: &[String],
) -> String {
    let mut parts = Vec::with_capacity(include_dirs.len() + c_flags.len() + cpp_flags.len() + 2);
    parts.push("i".to_string());
    parts.extend(
        include_dirs
            .iter()
            .map(|p| p.to_string_lossy().into_owned()),
    );
    parts.push("c".to_string());
    parts.extend(c_flags.iter().cloned());
    parts.push("cxx".to_string());
    parts.extend(cpp_flags.iter().cloned());
    parts.join("\x1f")
}

fn latest_mtime(paths: &[PathBuf]) -> Result<Option<std::time::SystemTime>> {
    let mut latest = None;
    for path in paths {
        let modified = std::fs::metadata(path)?.modified()?;
        latest = Some(match latest {
            Some(current) if current > modified => current,
            _ => modified,
        });
    }
    Ok(latest)
}

fn should_skip_failed_framework_lib(
    marker_path: &Path,
    signature: &str,
    sources: &[PathBuf],
) -> Result<bool> {
    if !marker_path.exists() {
        return Ok(false);
    }

    let marker_text = std::fs::read_to_string(marker_path)?;
    let recorded_signature = marker_text.lines().next().unwrap_or_default();
    if recorded_signature != signature {
        return Ok(false);
    }

    let Some(latest_source_time) = latest_mtime(sources)? else {
        return Ok(false);
    };
    let marker_time = std::fs::metadata(marker_path)?.modified()?;
    Ok(marker_time >= latest_source_time)
}

fn record_failed_framework_lib(marker_path: &Path, signature: &str, error: &str) {
    let _ = std::fs::write(marker_path, format!("{signature}\n{error}\n"));
}

fn build_fingerprint_path(build_dir: &Path) -> PathBuf {
    build_dir.join("build_fingerprint.json")
}

fn profile_label(profile: fbuild_core::BuildProfile) -> &'static str {
    match profile {
        fbuild_core::BuildProfile::Release => "release",
        fbuild_core::BuildProfile::Quick => "quick",
    }
}

fn collect_fast_path_watches(build_dir: &Path, project_dir: &Path) -> Vec<FingerprintWatch> {
    let mut watches = Vec::new();
    if let Some(watch) =
        crate::build_fingerprint::fast_path_watch("project", build_dir, project_dir)
    {
        watches.push(watch);
    }
    let resolved_libs_dir = build_dir.join("libs");
    if let Some(watch) =
        crate::build_fingerprint::fast_path_watch("dep_libs", build_dir, &resolved_libs_dir)
    {
        watches.push(watch);
    }
    watches
}

fn compile_db_is_current(build_dir: &Path, project_dir: &Path) -> bool {
    let build_copy = build_dir.join("compile_commands.json");
    if !build_copy.exists() {
        return false;
    }
    crate::compile_database::CompileDatabase::expected_output_path(build_dir, project_dir).exists()
}

fn expected_fast_path_artifacts(
    build_dir: &Path,
    project_dir: &Path,
) -> (PathBuf, PathBuf, PathBuf, PathBuf, PathBuf) {
    (
        build_dir.join("firmware.elf"),
        build_dir.join("firmware.bin"),
        build_dir.join("bootloader.bin"),
        build_dir.join("partitions.bin"),
        crate::compile_database::CompileDatabase::expected_output_path(build_dir, project_dir),
    )
}

impl BuildOrchestrator for Esp32Orchestrator {
    fn platform(&self) -> Platform {
        Platform::Espressif32
    }

    fn build(&self, params: &BuildParams) -> Result<BuildResult> {
        let start = Instant::now();
        // Env-gated per-phase timer (FBUILD_PERF_LOG=1); zero overhead when unset.
        let mut perf = crate::perf_log::PerfTimer::new("esp32-orchestrator");

        // 0. Discover zccache compiler cache (startup is deferred until compile work begins)
        let compiler_cache = {
            let _g = perf.phase("zccache-discover");
            crate::zccache::find_zccache().map(std::path::Path::to_path_buf)
        };

        // 1-2. Parse config, load board, setup build dirs, resolve src dir, collect flags
        let mut ctx = crate::pipeline::BuildContext::new_with_perf(params, Some(&mut perf))?;

        // 3. Load MCU config from embedded JSON
        let mut mcu_config = get_mcu_config(&ctx.board.mcu)?;

        tracing::info!(
            "ESP32 build: {} ({}, {})",
            ctx.board.name,
            ctx.board.mcu,
            mcu_config.architecture
        );

        // 4-6. Resolve platform, toolchain, and framework
        let _resolve_phase = perf.phase("pioarduino-resolve");
        let (toolchain, framework) =
            resolve_pioarduino_packages(&params.project_dir, &ctx.board.mcu, &mcu_config)?;
        drop(_resolve_phase);
        let _toolchain_cache_dir = fbuild_packages::Package::get_info(&toolchain).install_path;
        let _framework_cache_dir = fbuild_packages::Package::get_info(&framework).install_path;

        // Aliases for build dirs (already set up by BuildContext::new())
        let build_dir = &ctx.build_dir;
        let core_build_dir = &ctx.core_build_dir;
        let src_build_dir = &ctx.src_build_dir;

        // Read link-affecting config before the expensive include/library/source discovery steps
        // so the no-op fast path can return early on warm builds.
        let sdk_ld_flags = framework.get_sdk_ld_flags(&ctx.board.mcu);
        let sdk_defines = framework.get_sdk_defines(&ctx.board.mcu);

        if sdk_ld_flags.iter().any(|f| f == "-fno-lto") {
            mcu_config.disable_lto();
        }

        let mut user_flags = sdk_defines;
        let user_build_flags = ctx.config.get_build_flags(&params.env_name)?;
        user_flags.extend(user_build_flags.clone());
        let embed_files = ctx.config.get_embed_files(&params.env_name)?;
        let embed_txtfiles = ctx.config.get_embed_txtfiles(&params.env_name)?;

        let f_for_image = ctx
            .board
            .f_image
            .as_deref()
            .or(ctx.board.f_flash.as_deref());
        let flash_freq = crate::esp32::esp32_linker::f_flash_to_esptool_freq(
            f_for_image,
            mcu_config.default_flash_freq(),
        );
        let flash_mode = ctx
            .board
            .flash_mode
            .clone()
            .unwrap_or_else(|| mcu_config.default_flash_mode().to_string());
        let flash_size = crate::esp32::mcu_config::bytes_to_flash_size(
            ctx.board.max_flash,
            mcu_config.default_flash_size(),
        )
        .to_string();
        let metadata_hash = stable_hash_json(&Esp32FingerprintMetadata {
            version: BUILD_FINGERPRINT_VERSION,
            env_name: params.env_name.clone(),
            profile: profile_label(params.profile).to_string(),
            board_name: ctx.board.name.clone(),
            board_mcu: ctx.board.mcu.clone(),
            board_define: ctx.board.board.clone(),
            board_core: ctx.board.core.clone(),
            board_variant: ctx.board.variant.clone(),
            board_variant_h: ctx.board.variant_h.clone(),
            board_extra_flags: ctx.board.extra_flags.clone(),
            board_upload_protocol: ctx.board.upload_protocol.clone(),
            board_upload_speed: ctx.board.upload_speed.clone(),
            board_partitions: ctx.board.partitions.clone(),
            board_ldscript: ctx.board.ldscript.clone(),
            board_platform: ctx.board.platform_str.clone(),
            architecture: mcu_config.architecture.clone(),
            platform: "espressif32".to_string(),
            flash_mode: flash_mode.clone(),
            flash_freq: flash_freq.clone(),
            flash_size: flash_size.clone(),
            max_flash: ctx.board.max_flash,
            max_ram: ctx.board.max_ram,
        })?;
        let fingerprint_path = build_fingerprint_path(build_dir);
        let fingerprint_watches = {
            let _g = perf.phase("fp-watches-collect");
            collect_fast_path_watches(build_dir, &params.project_dir)
        };

        if !params.compiledb_only
            && !params.symbol_analysis
            && params.symbol_analysis_path.is_none()
        {
            let _fast_path_phase = perf.phase("fast-path-check");
            let (fast_elf, fast_bin, fast_boot, fast_parts, fast_compile_db) =
                expected_fast_path_artifacts(build_dir, &params.project_dir);
            let required_artifacts = [
                fast_elf.clone(),
                fast_bin.clone(),
                fast_boot.clone(),
                fast_parts.clone(),
                fast_compile_db.clone(),
            ];
            // ESP32 also requires the project-root copy of compile_commands.json
            // to be in sync with the build-dir copy. That's platform-specific,
            // so it rides on the shared helper via `extra_artifact_ok`.
            let compile_db_fresh = || compile_db_is_current(build_dir, &params.project_dir);
            let inputs = crate::build_fingerprint::FastPathInputs {
                fingerprint_path: &fingerprint_path,
                metadata_hash: &metadata_hash,
                watches: &fingerprint_watches,
                required_artifacts: &required_artifacts,
                extra_artifact_ok: Some(&compile_db_fresh),
                watch_set_cache: params.watch_set_cache.as_deref(),
                compiler_cache: compiler_cache.as_deref(),
            };
            if let Some(hit) = crate::build_fingerprint::fast_path_check(&inputs)? {
                ctx.build_log.push(
                    "No-op fingerprint matched; reusing existing ESP32 artifacts.".to_string(),
                );
                let elapsed = start.elapsed().as_secs_f64();
                return Ok(BuildResult {
                    success: true,
                    firmware_path: Some(fast_bin),
                    elf_path: Some(fast_elf),
                    size_info: hit.size_info,
                    symbol_map: None,
                    build_time_secs: elapsed,
                    message: format!(
                        "ESP32 ({}) build for {} reused cached artifacts",
                        ctx.board.mcu, params.env_name
                    ),
                    compile_database_path: Some(fast_compile_db),
                    build_log: ctx.build_log,
                });
            }
        }

        if let Some(ref zcc) = compiler_cache {
            crate::zccache::ensure_running(zcc);
        }

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

        let framework_dir = fbuild_packages::Package::ensure_installed(&framework)?;
        tracing::info!("ESP32 framework at {}", framework_dir.display());

        let tc_label = if mcu_config.is_riscv() {
            "riscv32-esp-elf-gcc"
        } else {
            "xtensa-esp-elf-gcc"
        };
        crate::pipeline::log_toolchain_version(
            &toolchain.get_gcc_path(),
            tc_label,
            &mut ctx.build_log,
        );

        let core_dir = framework.get_core_dir(&ctx.board.core);
        let variant_dir = framework.get_variant_dir(&ctx.board.variant);
        let sdk_memory_type = ctx
            .board
            .effective_esp32_memory_type(mcu_config.default_flash_mode());

        let mut include_dirs = vec![core_dir.clone()];
        if variant_dir.exists() {
            include_dirs.push(variant_dir.clone());
        }
        // Add SDK include paths (294+ paths from ESP-IDF)
        include_dirs
            .extend(framework.get_sdk_include_dirs(&ctx.board.mcu, sdk_memory_type.as_deref()));

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

        include_dirs.push(ctx.src_dir.clone());
        crate::pipeline::discover_project_includes(&params.project_dir, &mut include_dirs);
        // Toolchain sysroot includes (xtensa headers, etc.)
        include_dirs.extend(toolchain.get_include_dirs());

        // Read SDK flags early — needed to check LTO before compiling.
        let sdk_ld_flags = framework.get_sdk_ld_flags(&ctx.board.mcu);
        let sdk_lib_flags = framework.get_sdk_lib_flags(&ctx.board.mcu, sdk_memory_type.as_deref());
        let sdk_ld_scripts =
            LinkerScripts::from_raw_flags(&framework.get_sdk_ld_scripts(&ctx.board.mcu));
        let sdk_defines = framework.get_sdk_defines(&ctx.board.mcu);

        // If SDK specifies -fno-lto, disable LTO in MCU config profiles to avoid
        // compiling objects with LTO that the linker can't handle.
        if sdk_ld_flags.iter().any(|f| f == "-fno-lto") {
            mcu_config.disable_lto();
        }

        // 8.5. Library dependencies
        let lib_deps = ctx.config.get_lib_deps(&params.env_name)?;
        let lib_ignore = ctx.config.get_lib_ignore(&params.env_name)?;

        use fbuild_packages::Toolchain;
        let mut library_archives = Vec::new();

        // Read user build_flags early — needed for both library and sketch compilation.
        // SDK defines (from flags/defines) are prepended so user flags can override them.
        let mut user_flags = sdk_defines;
        let mut user_build_flags = ctx.config.get_build_flags(&params.env_name)?;
        user_build_flags.extend(params.extra_build_flags.clone());
        user_flags.extend(user_build_flags.clone());
        let user_overlay = LanguageExtraFlags {
            common: user_flags
                .iter()
                .cloned()
                .chain(ctx.global_compile_overlay.common.iter().cloned())
                .collect(),
            c: ctx.global_compile_overlay.c.clone(),
            cxx: ctx.global_compile_overlay.cxx.clone(),
            asm: ctx.global_compile_overlay.asm.clone(),
        };
        let src_overlay = LanguageExtraFlags::combined(&[
            &user_overlay,
            &LanguageExtraFlags {
                common: ctx.src_flags.clone(),
                c: Vec::new(),
                cxx: Vec::new(),
                asm: Vec::new(),
            },
            &ctx.project_compile_overlay,
        ]);

        // Emit a warning if CDC on boot is effectively enabled (may cause Serial to block
        // when no USB host is connected).
        warn_if_cdc_on_boot(
            &ctx.board.name,
            ctx.board.extra_flags.as_deref(),
            &user_build_flags,
        );
        crate::warn_debug_build_flags(&user_build_flags);

        if !lib_deps.is_empty() {
            let libs_dir = build_dir.join("libs");

            // Build compiler to get flags for library compilation
            let mut defines = ctx.board.get_defines();
            defines.extend(mcu_config.defines_map());

            let temp_compiler = Esp32Compiler::with_temp_dir(
                toolchain.get_gcc_path(),
                toolchain.get_gxx_path(),
                mcu_config.clone(),
                &ctx.board.f_cpu,
                defines.clone(),
                include_dirs.clone(),
                params.profile,
                params.verbose,
                build_dir.join("tmp"),
            )
            .with_build_unflags(ctx.build_unflags.clone());
            // Apply user build_flags to library compilation (matching PlatformIO behavior).
            // User flags like -std=gnu++2a replace the MCU config's -std=gnu++2b.
            let c_flags = apply_overlay_flags(&temp_compiler.c_flags(), &user_overlay, "dummy.c");
            let cpp_flags =
                apply_overlay_flags(&temp_compiler.cpp_flags(), &user_overlay, "dummy.cpp");

            let jobs = crate::parallel::effective_jobs(params.jobs);
            // Use gcc-ar for LTO archives so the linker-plugin index is written.
            let dep_ar_path = toolchain.get_ar_path();
            let dep_gcc_ar_path = toolchain.get_gcc_ar_path();
            let dep_lib_ar_path = crate::pipeline::pick_archiver(
                &dep_ar_path,
                &dep_gcc_ar_path,
                &c_flags,
                &cpp_flags,
            );
            let lib_result = fbuild_packages::library::library_manager::ensure_libraries_sync(
                &lib_deps,
                &lib_ignore,
                &toolchain.get_gcc_path(),
                &toolchain.get_gxx_path(),
                dep_lib_ar_path,
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

        // 8.5b. Project-as-library compilation — shared with sequential pipeline.
        // When the project root contains library.json or library.properties (e.g., FastLED),
        // the project's own src/ directory is compiled as a library archive so that example
        // sketches can link against it. Centralized in pipeline::compile_project_as_library
        // so every orchestrator gets this behavior architecturally.
        if !params.compiledb_only {
            // Build temp compiler to get the actual c_flags/cpp_flags ESP32 uses for
            // library compilation. SDK defines + user flags must be applied so the
            // archive matches what sketch sources see.
            let mut p_defines = ctx.board.get_defines();
            p_defines.extend(mcu_config.defines_map());
            let p_compiler = Esp32Compiler::with_temp_dir(
                toolchain.get_gcc_path(),
                toolchain.get_gxx_path(),
                mcu_config.clone(),
                &ctx.board.f_cpu,
                p_defines,
                include_dirs.clone(),
                params.profile,
                params.verbose,
                build_dir.join("tmp"),
            )
            .with_build_unflags(ctx.build_unflags.clone());
            let p_c_flags = apply_overlay_flags(&p_compiler.c_flags(), &src_overlay, "dummy.c");
            let p_cpp_flags =
                apply_overlay_flags(&p_compiler.cpp_flags(), &src_overlay, "dummy.cpp");

            // Collect lib/* names so the helper can detect collisions with project-as-library.
            let mut existing_lib_names = std::collections::HashSet::new();
            let local_lib_dir = params.project_dir.join("lib");
            if local_lib_dir.is_dir() {
                if let Ok(entries) = std::fs::read_dir(&local_lib_dir) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.is_dir() {
                            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                                existing_lib_names.insert(name.to_lowercase());
                            }
                        }
                    }
                }
            }

            let gcc_path = toolchain.get_gcc_path();
            let gxx_path = toolchain.get_gxx_path();
            let ar_path = toolchain.get_ar_path();
            let gcc_ar_path = toolchain.get_gcc_ar_path();
            // Use gcc-ar for LTO archives so the linker-plugin index is written.
            let lib_ar_path =
                crate::pipeline::pick_archiver(&ar_path, &gcc_ar_path, &p_c_flags, &p_cpp_flags);
            let lib_env = crate::pipeline::LibraryBuildEnv {
                gcc_path: &gcc_path,
                gxx_path: &gxx_path,
                ar_path: lib_ar_path,
                c_flags: &p_c_flags,
                cpp_flags: &p_cpp_flags,
                include_dirs: &include_dirs,
                verbose: params.verbose,
                jobs: crate::parallel::effective_jobs(params.jobs),
                compiler_cache: compiler_cache.as_deref(),
            };
            if let Some(archive) = crate::pipeline::compile_project_as_library(
                &params.project_dir,
                &ctx.src_dir,
                build_dir,
                &lib_env,
                &existing_lib_names,
            )? {
                library_archives.push(archive);
            }
        }

        tracing::info!("include paths: {} total", include_dirs.len());

        // 8.6. Compile framework built-in libraries (WiFi, FS, SPIFFS, Network, etc.)
        // The linker's --gc-sections will strip any unused code.
        // Skip when only generating compile_commands.json.
        if !params.compiledb_only {
            let fw_libs_started = Instant::now();
            perf.checkpoint("fw-libs-start");
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
                let mut fw_defines = ctx.board.get_defines();
                fw_defines.extend(mcu_config.defines_map());

                let fw_compiler = Esp32Compiler::with_temp_dir(
                    toolchain.get_gcc_path(),
                    toolchain.get_gxx_path(),
                    mcu_config.clone(),
                    &ctx.board.f_cpu,
                    fw_defines,
                    include_dirs.clone(),
                    params.profile,
                    params.verbose,
                    build_dir.join("tmp"),
                )
                .with_build_unflags(ctx.build_unflags.clone());
                let fw_c_flags =
                    apply_overlay_flags(&fw_compiler.c_flags(), &user_overlay, "dummy.c");
                let fw_cpp_flags =
                    apply_overlay_flags(&fw_compiler.cpp_flags(), &user_overlay, "dummy.cpp");
                let fw_signature = framework_signature(&include_dirs, &fw_c_flags, &fw_cpp_flags);

                let mut fw_lib_count = 0;
                let mut fw_lib_seen = 0;
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

                        fw_lib_seen += 1;

                        // Check if archive already exists
                        let archive_path = fw_libs_build_dir.join(format!("lib{}.a", lib_name));
                        if archive_path.exists() {
                            if perf.is_active() {
                                perf.checkpoint(format!(
                                    "fw-lib-cache-hit name={} index={}",
                                    lib_name, fw_lib_seen
                                ));
                            }
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
                        let failure_marker =
                            framework_failure_marker(&fw_libs_build_dir, &lib_name);
                        if should_skip_failed_framework_lib(
                            &failure_marker,
                            &fw_signature,
                            &sources,
                        )? {
                            if perf.is_active() {
                                perf.checkpoint(format!(
                                    "fw-lib-skip-failed name={} index={} sources={}",
                                    lib_name,
                                    fw_lib_seen,
                                    sources.len()
                                ));
                            }
                            tracing::debug!(
                                "skipping previously failed framework library '{}'",
                                lib_name
                            );
                            continue;
                        }

                        let fw_jobs = crate::parallel::effective_jobs(params.jobs);
                        if perf.is_active() {
                            perf.checkpoint(format!(
                                "fw-lib-compile-start name={} index={} sources={} jobs={}",
                                lib_name,
                                fw_lib_seen,
                                sources.len(),
                                fw_jobs
                            ));
                        }
                        // Use gcc-ar for LTO archives so the linker-plugin index is written.
                        let fw_ar_path = toolchain.get_ar_path();
                        let fw_gcc_ar_path = toolchain.get_gcc_ar_path();
                        let fw_lib_ar_path = crate::pipeline::pick_archiver(
                            &fw_ar_path,
                            &fw_gcc_ar_path,
                            &fw_c_flags,
                            &fw_cpp_flags,
                        );
                        match fbuild_packages::library::library_compiler::compile_library_with_jobs(
                            &lib_name,
                            &sources,
                            &include_dirs,
                            &toolchain.get_gcc_path(),
                            &toolchain.get_gxx_path(),
                            fw_lib_ar_path,
                            &fw_c_flags,
                            &fw_cpp_flags,
                            &fw_libs_build_dir,
                            params.verbose,
                            fw_jobs,
                            compiler_cache.as_deref(),
                        ) {
                            Ok(Some(archive)) => {
                                let _ = std::fs::remove_file(&failure_marker);
                                library_archives.push(archive);
                                fw_lib_count += 1;
                                if perf.is_active() {
                                    perf.checkpoint(format!(
                                        "fw-lib-compile-finish name={} index={} count={}",
                                        lib_name, fw_lib_seen, fw_lib_count
                                    ));
                                }
                            }
                            Ok(None) => {
                                if perf.is_active() {
                                    perf.checkpoint(format!(
                                        "fw-lib-header-only name={} index={}",
                                        lib_name, fw_lib_seen
                                    ));
                                }
                            }
                            Err(e) => {
                                // Non-fatal: some framework libs may fail to compile
                                // (e.g., platform-specific ones). The linker will report
                                // if any actually-needed symbols are missing.
                                if perf.is_active() {
                                    perf.checkpoint(format!(
                                        "fw-lib-compile-error name={} index={}",
                                        lib_name, fw_lib_seen
                                    ));
                                }
                                tracing::debug!(
                                    "framework library {} failed to compile: {}",
                                    lib_name,
                                    e
                                );
                                record_failed_framework_lib(
                                    &failure_marker,
                                    &fw_signature,
                                    &e.to_string(),
                                );
                            }
                        }
                    }
                }

                if fw_lib_count > 0 {
                    tracing::info!("compiled {} framework built-in libraries", fw_lib_count);
                }
            }
            perf.record("fw-libs", fw_libs_started.elapsed());
            perf.checkpoint("fw-libs-finish");
        }

        // 9. Scan sources
        let sources = {
            let _g = perf.phase("scan-sources");
            let scanner = SourceScanner::new(&ctx.src_dir, src_build_dir);
            let variant_dir_opt = if variant_dir.exists() {
                Some(variant_dir.as_path())
            } else {
                None
            };
            scanner.scan_all_filtered(
                Some(&core_dir),
                variant_dir_opt,
                ctx.source_filter.as_deref(),
            )?
        };

        tracing::info!(
            "sources: {} sketch, {} core, {} variant",
            sources.sketch_sources.len(),
            sources.core_sources.len(),
            sources.variant_sources.len(),
        );

        // 10-11. Compile
        let mut defines = ctx.board.get_defines();
        defines.extend(mcu_config.defines_map());

        // Defines required by the new framework (3.3.7+).
        // Use \" escapes for GCC response file compatibility (see ctx.board.rs).
        defines
            .entry("ARDUINO_BOARD".to_string())
            .or_insert_with(|| format!("\\\"{}\\\"", ctx.board.name));
        defines
            .entry("ARDUINO_VARIANT".to_string())
            .or_insert_with(|| format!("\\\"{}\\\"", ctx.board.variant));

        let compiler = Esp32Compiler::with_temp_dir(
            toolchain.get_gcc_path(),
            toolchain.get_gxx_path(),
            mcu_config.clone(),
            &ctx.board.f_cpu,
            defines,
            include_dirs.clone(),
            params.profile,
            params.verbose,
            build_dir.join("tmp"),
        )
        .with_build_unflags(ctx.build_unflags.clone());
        let jobs = crate::parallel::effective_jobs(params.jobs);
        tracing::info!("parallel compilation: {} jobs", jobs);

        // Build source lists and flags needed for compile_commands.json
        let mut all_core_sources: Vec<std::path::PathBuf> = Vec::new();
        all_core_sources.extend(sources.core_sources.iter().cloned());
        all_core_sources.extend(sources.variant_sources.iter().cloned());

        // Precompute values needed for compile_commands.json in both paths
        let include_flags = compiler.base.build_include_flags();
        let arch = if mcu_config.is_xtensa() {
            crate::compile_database::TargetArchitecture::Xtensa
        } else {
            crate::compile_database::TargetArchitecture::Riscv32
        };

        // compiledb_only: generate compile_commands.json without compiling
        if params.compiledb_only {
            let compile_database_path = {
                let _g = perf.phase("compile-db");
                crate::pipeline::generate_compile_db(
                    compiler.gcc_path(),
                    compiler.gxx_path(),
                    &compiler.c_flags(),
                    &compiler.cpp_flags(),
                    &include_flags,
                    &user_overlay,
                    &src_overlay,
                    &all_core_sources,
                    &sources.sketch_sources,
                    core_build_dir,
                    src_build_dir,
                    build_dir,
                    &params.project_dir,
                    arch,
                )?
            };
            let elapsed = start.elapsed().as_secs_f64();
            return Ok(BuildResult {
                success: true,
                firmware_path: None,
                elf_path: None,
                size_info: None,
                symbol_map: None,
                build_time_secs: elapsed,
                message: format!(
                    "compile_commands.json generated for {} ({})",
                    params.env_name, ctx.board.mcu
                ),
                compile_database_path,
                build_log: ctx.build_log,
            });
        }

        // Compile core + variant sources in parallel
        let build_log_mutex = std::sync::Mutex::new(ctx.build_log);
        let core_result = {
            let _g = perf.phase("compile-core-variant");
            crate::parallel::compile_sources_parallel(
                &compiler,
                &all_core_sources,
                core_build_dir,
                &user_overlay,
                jobs,
                Some(&build_log_mutex),
            )?
        };

        // Compile sketch sources in parallel
        let sketch_result = {
            let _g = perf.phase("compile-sketch");
            crate::parallel::compile_sources_parallel(
                &compiler,
                &sources.sketch_sources,
                src_build_dir,
                &src_overlay,
                jobs,
                Some(&build_log_mutex),
            )?
        };

        // Unwrap build log and flush collected warnings
        let mut build_log = build_log_mutex.into_inner().unwrap();
        for w in core_result.warnings.iter().chain(&sketch_result.warnings) {
            crate::build_output::collect_warnings(w, &mut build_log);
        }

        let core_objects = core_result.objects;
        let mut sketch_objects = sketch_result.objects;

        // Compile local libraries from the project's lib/ directory.
        // PlatformIO discovers and compiles these automatically.
        {
            let _g = perf.phase("compile-local-libs");
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

                        let lib_info =
                            fbuild_packages::library::library_info::InstalledLibrary::new(
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

                        // Use gcc-ar for LTO archives so the linker-plugin index is written.
                        let local_ar_path = toolchain.get_ar_path();
                        let local_gcc_ar_path = toolchain.get_gcc_ar_path();
                        let local_c_flags =
                            apply_overlay_flags(&compiler.c_flags(), &src_overlay, "dummy.c");
                        let local_cpp_flags =
                            apply_overlay_flags(&compiler.cpp_flags(), &src_overlay, "dummy.cpp");
                        let local_lib_ar_path = crate::pipeline::pick_archiver(
                            &local_ar_path,
                            &local_gcc_ar_path,
                            &local_c_flags,
                            &local_cpp_flags,
                        );
                        match fbuild_packages::library::library_compiler::compile_library_with_jobs(
                            &lib_name,
                            &lib_sources,
                            &include_dirs,
                            &toolchain.get_gcc_path(),
                            &toolchain.get_gxx_path(),
                            local_lib_ar_path,
                            &local_c_flags,
                            &local_cpp_flags,
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
        }

        // 11.5. Process embedded files (board_build.embed_files + embed_txtfiles)
        //
        // `.lnk` entries are pre-resolved: each `.lnk` is parsed, its blob is
        // fetched (or pulled from the disk cache), and the materialized path
        // is substituted in place before objcopy sees it. The `_lnk_leases`
        // vector keeps cache leases alive until we leave this scope, so the
        // disk-cache GC can't reap a blob mid-build.
        if !embed_files.is_empty() || !embed_txtfiles.is_empty() {
            let _g = perf.phase("embed-files");
            let embed_dir = build_dir.join("embed");
            std::fs::create_dir_all(&embed_dir)?;

            let lnk_dir = embed_dir.join("lnk");
            let mut _lnk_leases: Vec<fbuild_packages::lnk::MaterializedLnk> = Vec::new();
            let lnk_cache = fbuild_packages::DiskCache::open().ok();

            let expand = |entries: &[String]| -> Result<Vec<String>> {
                let mut out = Vec::with_capacity(entries.len());
                for entry in entries {
                    let p = if Path::new(entry).is_absolute() {
                        std::path::PathBuf::from(entry)
                    } else {
                        params.project_dir.join(entry)
                    };
                    if fbuild_packages::lnk::has_lnk_extension(&p) {
                        let cache = lnk_cache.as_ref().ok_or_else(|| {
                            fbuild_core::FbuildError::PackageError(
                                "disk cache unavailable; cannot resolve .lnk entries".to_string(),
                            )
                        })?;
                        let m = fbuild_packages::lnk::materialize_lnk_entry(&p, &lnk_dir, cache)?;
                        out.push(m.target_path.to_string_lossy().into_owned());
                    } else {
                        out.push(entry.clone());
                    }
                }
                Ok(out)
            };
            let resolved_embed_files = expand(&embed_files)?;
            let resolved_embed_txtfiles = expand(&embed_txtfiles)?;

            let objcopy_path = toolchain.get_objcopy_path();
            let (output_target, binary_arch) = if mcu_config.is_riscv() {
                ("elf32-littleriscv", "riscv")
            } else {
                ("elf32-xtensa-le", "xtensa")
            };

            let embed_objects = process_embed_files(
                &resolved_embed_files,
                &resolved_embed_txtfiles,
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
        let compile_database_path = {
            let _g = perf.phase("compile-db");
            crate::pipeline::generate_compile_db(
                compiler.gcc_path(),
                compiler.gxx_path(),
                &compiler.c_flags(),
                &compiler.cpp_flags(),
                &include_flags,
                &user_overlay,
                &src_overlay,
                &all_core_sources,
                &sources.sketch_sources,
                core_build_dir,
                src_build_dir,
                build_dir,
                &params.project_dir,
                arch,
            )?
        };

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
            Some(flash_mode.clone()),
            &flash_freq,
            ctx.board.max_flash,
            ctx.board.max_ram,
            params.verbose,
        );

        let link_result = {
            let _g = perf.phase("link-convert-size");
            crate::linker::Linker::link_all(
                &linker,
                &sketch_objects,
                &all_archives,
                build_dir,
                &crate::linker::LinkExtraArgs {
                    flags: ctx.overlay_link_flags.clone(),
                    libs: ctx.overlay_link_libs.clone(),
                },
                params.symbol_analysis,
            )?
        };

        // 14. Prepare boot artifacts for deployment / emulation
        let boot_artifacts_started = Instant::now();
        perf.checkpoint("boot-artifacts-start");
        let boot_dst = build_dir.join("bootloader.bin");
        let boot_bin_src = framework.get_bootloader_bin(&ctx.board.mcu);
        if boot_bin_src.exists() {
            // Pre-built bootloader.bin available — just copy
            std::fs::copy(&boot_bin_src, &boot_dst)?;
            tracing::info!("copied bootloader.bin");
        } else {
            // Convert bootloader ELF to BIN using esptool elf2image.
            //
            // CRITICAL: The ESP32 ROM bootloader can only fetch the second-stage
            // bootloader from flash in DIO mode (or OPI mode for octal-SPI
            // boards). Even if the application is QIO, the bootloader itself
            // must be DIO — otherwise the ROM bootloader cannot read it and
            // the chip enters a watchdog reset loop with `Saved PC` pointing
            // into ROM (e.g. `0x400454d5` on ESP32-S3).
            //
            // The Arduino-ESP32 framework ships pre-built ELFs named
            // `bootloader_<mode>_<freq>.elf` for this exact reason; we pick
            // `bootloader_dio_80m.elf` for non-OPI boards and pass
            // `--flash-mode dio` to esptool so the resulting BIN header
            // (byte 0x02) has the correct mode. The application's flash
            // mode (which may be QIO/QOUT/etc) is unaffected — that mode
            // is encoded in the firmware.bin and applied later by the
            // second-stage bootloader.
            //
            // We treat the app's `flash_mode` as OPI iff it equals "opi";
            // every other value (qio, qout, dio, dout, undefined) maps to
            // a DIO bootloader. This matches Arduino-ESP32 / PlatformIO
            // behaviour. Frequency is taken as-is because the bootloader
            // ELFs only exist at 80m (and 120m for QIO chips), and the
            // ROM bootloader runs at the boot frequency anyway.
            let app_flash_mode = ctx
                .board
                .flash_mode
                .as_deref()
                .unwrap_or(mcu_config.default_flash_mode());
            let boot_flash_mode = if app_flash_mode == "opi" {
                "opi"
            } else {
                "dio"
            };
            let boot_elf =
                framework.get_bootloader_elf(&ctx.board.mcu, boot_flash_mode, &flash_freq);
            if boot_elf.exists() {
                let boot_elf_str = boot_elf.to_string_lossy();
                let boot_dst_str = boot_dst.to_string_lossy();
                let flash_size = crate::esp32::mcu_config::bytes_to_flash_size(
                    ctx.board.max_flash,
                    mcu_config.default_flash_size(),
                );
                let args = [
                    "esptool",
                    "--chip",
                    &ctx.board.mcu,
                    "elf2image",
                    "--flash-mode",
                    boot_flash_mode,
                    "--flash-freq",
                    &flash_freq,
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
        let parts_bin_src = framework.get_partitions_bin(&ctx.board.mcu);
        if parts_bin_src.exists() {
            // Pre-built partitions.bin available — just copy
            std::fs::copy(&parts_bin_src, &parts_dst)?;
            tracing::info!("copied partitions.bin");
        } else {
            // Generate partitions.bin from CSV using gen_esp32part.py
            let partitions_name = ctx.board.partitions.as_deref().unwrap_or("default.csv");
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

        let boot_app0_src = framework.get_boot_app0_bin();
        let boot_app0_dst = build_dir.join("boot_app0.bin");
        if boot_app0_src.exists() {
            std::fs::copy(&boot_app0_src, &boot_app0_dst)?;
            tracing::info!("copied boot_app0.bin");
        }
        perf.record("boot-artifacts", boot_artifacts_started.elapsed());
        perf.checkpoint("boot-artifacts-finish");

        // 15. Size reporting + result assembly
        let fingerprint_started = Instant::now();
        perf.checkpoint("fingerprint-save-start");
        let persisted_fingerprint = PersistedBuildFingerprint {
            version: BUILD_FINGERPRINT_VERSION,
            metadata_hash: metadata_hash.clone(),
            file_set_hash: match hash_watch_set_stamps_cached(
                &fingerprint_watches,
                params.watch_set_cache.as_deref(),
            ) {
                Ok(hash) => Some(hash),
                Err(e) => {
                    tracing::warn!("failed to hash watched inputs for fingerprint save: {}", e);
                    None
                }
            },
            size_info: link_result.size_info.clone(),
        };
        if let Err(e) = save_json(&fingerprint_path, &persisted_fingerprint) {
            tracing::warn!("failed to write build fingerprint: {}", e);
        }
        if let Some(ref zcc) = compiler_cache {
            for watch in &fingerprint_watches {
                if let Err(e) = crate::zccache::mark_fingerprint_success(zcc, watch) {
                    tracing::warn!(
                        "failed to mark zccache fingerprint success for {}: {}",
                        watch.root.display(),
                        e
                    );
                }
            }
        }
        perf.record("fingerprint-save", fingerprint_started.elapsed());
        perf.checkpoint("fingerprint-save-finish");

        crate::pipeline::handle_link_result(
            &link_result,
            &mut build_log,
            params.symbol_analysis_path.as_deref(),
            params.verbose,
        );
        let elapsed = start.elapsed().as_secs_f64();
        let platform_label = format!("ESP32 ({})", ctx.board.mcu);
        Ok(crate::pipeline::assemble_build_result(
            link_result,
            elapsed,
            &platform_label,
            &params.env_name,
            compile_database_path,
            build_log,
        ))
    }
}

/// Apply user build_flags from platformio.ini onto base compiler flags.
///
/// Matches PlatformIO behavior: user flags are appended to common flags,
/// but `-std=` flags replace the existing standard (not stack). `-D` flags are
/// deduplicated by macro name so later values override earlier defaults without
/// tripping GCC redefinition warnings.
fn apply_user_flags(base_flags: &[String], user_flags: &[String]) -> Vec<String> {
    let mut result = base_flags.to_vec();
    for flag in user_flags {
        if flag.starts_with("-std=") {
            // Replace any existing -std= flag
            result.retain(|f| !f.starts_with("-std="));
        } else if let Some(define_name) = define_flag_name(flag) {
            // Replace any existing -DNAME / -DNAME=value flag with the same macro name.
            result.retain(|f| define_flag_name(f) != Some(define_name));
        }
        result.push(flag.clone());
    }
    result
}

fn apply_overlay_flags(
    base_flags: &[String],
    overlay: &LanguageExtraFlags,
    probe_name: &str,
) -> Vec<String> {
    apply_user_flags(base_flags, &overlay.for_source(Path::new(probe_name)))
}

fn define_flag_name(flag: &str) -> Option<&str> {
    let define = flag.strip_prefix("-D")?;
    let name = define
        .split_once('=')
        .map_or(define, |(name, _)| name)
        .trim();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
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

    // Ensure MCU-specific skeleton libs (e.g. ESP32-C2, ESP32-C61).
    // Some MCUs ship their SDK in a separate skeleton package.
    let mcu_suffix = mcu.strip_prefix("esp32").unwrap_or("");
    if !mcu_suffix.is_empty() {
        let skeleton_name = format!("framework-arduino-{}-skeleton-lib", mcu_suffix);
        if let Ok(skeleton_url) = platform.get_package_url(&skeleton_name) {
            framework.ensure_mcu_libs(&skeleton_url, mcu)?;
        }
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
    crate::pipeline::is_platform_project(project_dir, env_name, fbuild_core::Platform::Espressif32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_apply_user_flags_replaces_std_flag() {
        let base = vec!["-Os".to_string(), "-std=gnu++2b".to_string()];
        let user = vec!["-std=gnu++20".to_string()];

        let result = apply_user_flags(&base, &user);

        assert_eq!(result, vec!["-Os", "-std=gnu++20"]);
    }

    #[test]
    fn test_apply_user_flags_replaces_define_with_same_name() {
        let base = vec![
            r#"-DIDF_VER=\"v5.5.1-710-g8410210c9a\""#.to_string(),
            r#"-DESP_MDNS_VERSION_NUMBER=\"1.9.0\""#.to_string(),
            "-Os".to_string(),
        ];
        let user = vec![
            r#"-DESP_MDNS_VERSION_NUMBER=\"1.9.1\""#.to_string(),
            r#"-DIDF_VER=\"v5.5.2-729-g87912cd291\""#.to_string(),
        ];

        let result = apply_user_flags(&base, &user);

        assert_eq!(
            result,
            vec![
                "-Os",
                r#"-DESP_MDNS_VERSION_NUMBER=\"1.9.1\""#,
                r#"-DIDF_VER=\"v5.5.2-729-g87912cd291\""#,
            ]
        );
    }

    #[test]
    fn test_apply_user_flags_replaces_bare_define_with_value_define() {
        let base = vec!["-DFOO".to_string(), "-DBAR=1".to_string()];
        let user = vec!["-DFOO=2".to_string()];

        let result = apply_user_flags(&base, &user);

        assert_eq!(result, vec!["-DBAR=1", "-DFOO=2"]);
    }

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

    #[test]
    fn test_framework_signature_changes_with_flags() {
        let includes = vec![PathBuf::from("C:/sdk/include")];
        let sig_a = framework_signature(
            &includes,
            &["-O2".to_string()],
            &["-std=gnu++17".to_string()],
        );
        let sig_b = framework_signature(
            &includes,
            &["-O0".to_string()],
            &["-std=gnu++17".to_string()],
        );
        assert_ne!(sig_a, sig_b);
    }

    #[test]
    fn test_apply_user_flags_replaces_existing_define_by_key() {
        let merged = apply_user_flags(
            &[r#"-DIDF_VER=\"old\""#.to_string(), "-O2".to_string()],
            &[r#"-DIDF_VER=\"new\""#.to_string()],
        );
        assert_eq!(
            merged,
            vec![r#"-O2"#.to_string(), r#"-DIDF_VER=\"new\""#.to_string()]
        );
    }

    #[test]
    fn test_apply_user_flags_keeps_last_user_define() {
        let merged = apply_user_flags(
            &[],
            &[
                r#"-DMBEDTLS_CONFIG_FILE=\"a.h\""#.to_string(),
                r#"-DMBEDTLS_CONFIG_FILE=\"b.h\""#.to_string(),
            ],
        );
        assert_eq!(merged, vec![r#"-DMBEDTLS_CONFIG_FILE=\"b.h\""#.to_string()]);
    }

    #[test]
    fn test_skip_failed_framework_lib_when_marker_matches_and_is_current() {
        let tmp = tempfile::TempDir::new().unwrap();
        let source = tmp.path().join("Matter.cpp");
        std::fs::write(&source, "int x;").unwrap();
        let marker = framework_failure_marker(tmp.path(), "matter");
        let sig = framework_signature(&[], &["-O2".to_string()], &["-std=gnu++2b".to_string()]);
        std::thread::sleep(Duration::from_millis(20));
        record_failed_framework_lib(&marker, &sig, "compile failed");

        assert!(should_skip_failed_framework_lib(&marker, &sig, &[source]).unwrap());
    }

    #[test]
    fn test_retry_failed_framework_lib_after_source_change() {
        let tmp = tempfile::TempDir::new().unwrap();
        let source = tmp.path().join("Matter.cpp");
        std::fs::write(&source, "int x;").unwrap();
        let marker = framework_failure_marker(tmp.path(), "matter");
        let sig = framework_signature(&[], &["-O2".to_string()], &["-std=gnu++2b".to_string()]);
        std::thread::sleep(Duration::from_millis(20));
        record_failed_framework_lib(&marker, &sig, "compile failed");
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&source, "int y;").unwrap();

        assert!(!should_skip_failed_framework_lib(&marker, &sig, &[source]).unwrap());
    }

    #[test]
    fn test_retry_failed_framework_lib_after_signature_change() {
        let tmp = tempfile::TempDir::new().unwrap();
        let source = tmp.path().join("Matter.cpp");
        std::fs::write(&source, "int x;").unwrap();
        let marker = framework_failure_marker(tmp.path(), "matter");
        let sig_a = framework_signature(&[], &["-O2".to_string()], &["-std=gnu++2b".to_string()]);
        let sig_b = framework_signature(&[], &["-O0".to_string()], &["-std=gnu++2b".to_string()]);
        std::thread::sleep(Duration::from_millis(20));
        record_failed_framework_lib(&marker, &sig_a, "compile failed");

        assert!(!should_skip_failed_framework_lib(&marker, &sig_b, &[source]).unwrap());
    }
}
