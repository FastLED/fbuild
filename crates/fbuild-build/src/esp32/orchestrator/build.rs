//! `impl BuildOrchestrator for Esp32Orchestrator` — the high-level build flow.
//!
//! Most heavy work delegates to sibling submodules (`packages`, `framework_libs`,
//! `local_libs`, `boot_artifacts`, `embed_stage`, `helpers`).

use std::time::Instant;

use fbuild_core::{Platform, Result};
use fbuild_packages::Framework;

use super::super::esp32_compiler::Esp32Compiler;
use super::super::esp32_linker::Esp32Linker;
use super::super::mcu_config::get_mcu_config;
use super::boot_artifacts::prepare_boot_artifacts;
use super::cdc::warn_if_cdc_on_boot;
use super::embed_stage::stage_embed_files;
use super::fingerprint::Esp32FingerprintMetadata;
use super::framework_libs::compile_framework_builtin_libs;
use super::helpers::{apply_overlay_flags, compile_db_is_current, profile_label};
use super::local_libs::compile_local_libraries;
use super::packages::resolve_pioarduino_packages;
use super::Esp32Orchestrator;

use crate::build_fingerprint::{
    expected_fast_path_artifacts, stable_hash_json, FastPathCheckInputs, FastPathContract,
    FastPathPersistInputs, BUILD_FINGERPRINT_VERSION,
};
use crate::compiler::Compiler as _;
use crate::flag_overlay::LanguageExtraFlags;
use crate::linker::LinkerScripts;
use crate::{BuildOrchestrator, BuildParams, BuildResult, SourceScanner};

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

        // Compute eh_frame strip policy once per build (FastLED/fbuild#243).
        // Reads sdkconfig from the project dir (ESP32 only) so panic-backtrace
        // / gdbstub users automatically Preserve.
        let sdkconfig =
            fbuild_config::sdkconfig::SdkConfigSummary::from_project_dir(&params.project_dir);
        let eh_frame_policy = crate::eh_frame_policy_compute::compute_eh_frame_policy(
            &ctx,
            params.profile,
            Some(&sdkconfig),
        );

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

        // SDK directory selector: matches the chip's ROM revision (e.g.
        // `esp32p4_es` for ESP32-P4 eco0–eco2). Falls back to `mcu`.
        let sdk_variant = ctx.board.sdk_variant().to_string();

        // Read link-affecting config before the expensive include/library/source discovery steps
        // so the no-op fast path can return early on warm builds.
        let sdk_ld_flags = framework.get_sdk_ld_flags(&sdk_variant);
        let sdk_defines = framework.get_sdk_defines(&sdk_variant);

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
            board_chip_variant: ctx.board.chip_variant.clone(),
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
            eh_frame_policy: match eh_frame_policy {
                crate::eh_frame_policy::EhFramePolicy::Strip => "strip",
                crate::eh_frame_policy::EhFramePolicy::Preserve => "preserve",
            },
        })?;
        let (fast_elf, [fast_bin, fast_boot, fast_parts, fast_app0], fast_compile_db) =
            expected_fast_path_artifacts(
                build_dir,
                &params.project_dir,
                [
                    "firmware.bin",
                    "bootloader.bin",
                    "partitions.bin",
                    "boot_app0.bin",
                ],
            );
        let fast_path = {
            let _g = perf.phase("fp-watches-collect");
            FastPathContract::for_project_outputs(
                build_dir,
                &params.project_dir,
                [
                    fast_elf.clone(),
                    fast_bin.clone(),
                    fast_boot.clone(),
                    fast_parts.clone(),
                    fast_app0.clone(),
                    fast_compile_db.clone(),
                ],
            )
        };

        if !params.compiledb_only
            && !params.symbol_analysis
            && params.symbol_analysis_path.is_none()
        {
            let _fast_path_phase = perf.phase("fast-path-check");
            // ESP32 also requires the project-root copy of compile_commands.json
            // to be in sync with the build-dir copy. That's platform-specific,
            // so it rides on the shared helper via `extra_artifact_ok`.
            let compile_db_fresh = || compile_db_is_current(build_dir, &params.project_dir);
            let inputs = FastPathCheckInputs {
                metadata_hash: &metadata_hash,
                extra_artifact_ok: Some(&compile_db_fresh),
                watch_set_cache: params.watch_set_cache.as_deref(),
                compiler_cache: compiler_cache.as_deref(),
            };
            if let Some(hit) = crate::build_fingerprint::fast_path_check(&fast_path, &inputs)? {
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
            .extend(framework.get_sdk_include_dirs(&sdk_variant, sdk_memory_type.as_deref()));

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
        let sdk_ld_flags = framework.get_sdk_ld_flags(&sdk_variant);
        let sdk_lib_flags = framework.get_sdk_lib_flags(&sdk_variant, sdk_memory_type.as_deref());
        let sdk_ld_scripts =
            LinkerScripts::from_raw_flags(&framework.get_sdk_ld_scripts(&sdk_variant));
        let sdk_defines = framework.get_sdk_defines(&sdk_variant);

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
            .with_build_unflags(ctx.build_unflags.clone())
            .with_eh_frame_policy(eh_frame_policy);
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
            .with_build_unflags(ctx.build_unflags.clone())
            .with_eh_frame_policy(eh_frame_policy);
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
            compile_framework_builtin_libs(
                params,
                &mut perf,
                &framework,
                &toolchain,
                &mcu_config,
                &ctx.board,
                &ctx.build_unflags,
                eh_frame_policy,
                &include_dirs,
                &user_overlay,
                build_dir,
                compiler_cache.as_deref(),
                &mut library_archives,
            )?;
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
        .with_build_unflags(ctx.build_unflags.clone())
        .with_eh_frame_policy(eh_frame_policy);
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
        let core_cache = if params.clean {
            None
        } else {
            Some(crate::framework_core_cache::FrameworkCoreCache::new(
                &params.project_dir,
                "esp32",
                &params.env_name,
                params.profile,
                &compiler,
                &all_core_sources,
                &user_overlay,
            ))
        };
        if let Some(cache) = core_cache.as_ref() {
            let _g = perf.phase("core-cache-hydrate");
            match cache.hydrate(core_build_dir) {
                Ok(stats) if stats.copied > 0 || stats.skipped > 0 => tracing::info!(
                    "framework core cache hydrate key={} copied={} skipped={} from {}",
                    cache.key(),
                    stats.copied,
                    stats.skipped,
                    cache.path().display()
                ),
                Ok(_) => tracing::debug!(
                    "framework core cache miss key={} at {}",
                    cache.key(),
                    cache.path().display()
                ),
                Err(e) => tracing::warn!(
                    "framework core cache hydrate failed key={} at {}: {}",
                    cache.key(),
                    cache.path().display(),
                    e
                ),
            }
        }
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
        if let Some(cache) = core_cache.as_ref() {
            let _g = perf.phase("core-cache-store");
            match cache.store(core_build_dir) {
                Ok(stats) if stats.copied > 0 => tracing::info!(
                    "framework core cache store key={} copied={} to {}",
                    cache.key(),
                    stats.copied,
                    cache.path().display()
                ),
                Ok(_) => tracing::debug!(
                    "framework core cache store key={} had no new artifacts",
                    cache.key()
                ),
                Err(e) => tracing::warn!(
                    "framework core cache store failed key={} at {}: {}",
                    cache.key(),
                    cache.path().display(),
                    e
                ),
            }
        }

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
            compile_local_libraries(
                &params.project_dir,
                build_dir,
                &compiler,
                &toolchain,
                &include_dirs,
                &src_overlay,
                jobs,
                params.verbose,
                compiler_cache.as_deref(),
                &mut library_archives,
            )?;
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
            let objcopy_path = toolchain.get_objcopy_path();
            let embed_objects = stage_embed_files(
                &embed_files,
                &embed_txtfiles,
                &params.project_dir,
                build_dir,
                &objcopy_path,
                &mcu_config,
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
        prepare_boot_artifacts(
            build_dir,
            &framework,
            &ctx.board,
            &mcu_config,
            &flash_freq,
            &mut perf,
        )?;

        // 15. Size reporting + result assembly
        let fingerprint_started = Instant::now();
        perf.checkpoint("fingerprint-save-start");
        let fast_path_ready = fast_path
            .required_artifacts()
            .iter()
            .all(|path| path.exists())
            && compile_db_is_current(build_dir, &params.project_dir);
        if fast_path_ready {
            crate::build_fingerprint::persist_fast_path_success(
                &fast_path,
                &FastPathPersistInputs {
                    metadata_hash: &metadata_hash,
                    size_info: link_result.size_info.clone(),
                    watch_set_cache: params.watch_set_cache.as_deref(),
                    compiler_cache: compiler_cache.as_deref(),
                },
            );
        } else {
            tracing::warn!(
                "skipping ESP32 fast-path persistence because final artifacts are incomplete"
            );
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
