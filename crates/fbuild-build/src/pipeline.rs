//! Shared build pipeline helpers used by all platform orchestrators.
//!
//! Extracts the duplicated config-parse → board-load → build-dir-setup → compile → link
//! sequence that was copy-pasted across AVR, Teensy, and ESP32 orchestrators.

use std::path::{Path, PathBuf};
use std::time::Instant;

use fbuild_core::{BuildLog, Result};

use crate::compile_database::{self, CompileDatabase, TargetArchitecture};
use crate::compiler::Compiler;
use crate::flag_overlay::LanguageExtraFlags;
use crate::linker::LinkResult;
use crate::source_scanner::SourceCollection;
use crate::{BuildParams, BuildResult};

/// Common build state initialized at the start of every platform's `build()` method.
///
/// Created by [`BuildContext::new()`], which handles config parsing, board loading,
/// build directory setup, source directory resolution, and user flag collection.
pub struct BuildContext {
    pub config: fbuild_config::PlatformIOConfig,
    pub board: fbuild_config::BoardConfig,
    pub build_log: BuildLog,
    pub build_dir: PathBuf,
    pub core_build_dir: PathBuf,
    pub src_build_dir: PathBuf,
    pub src_dir: PathBuf,
    pub source_filter: Option<String>,
    pub user_flags: Vec<String>,
    pub src_flags: Vec<String>,
    pub all_src_flags: Vec<String>,
    pub global_compile_overlay: LanguageExtraFlags,
    pub project_compile_overlay: LanguageExtraFlags,
    pub overlay_link_flags: Vec<String>,
    pub overlay_link_libs: Vec<String>,
}

impl BuildContext {
    /// Parse platformio.ini, load board config, setup build directories,
    /// resolve source directory, and collect user flags.
    ///
    /// Takes `&BuildParams` so that new fields (e.g. `src_dir`) flow through
    /// automatically — orchestrators just pass `params` without listing every field.
    pub fn new(params: &BuildParams) -> Result<Self> {
        let project_dir = &params.project_dir;
        let env_name = &params.env_name;

        // 1. Parse platformio.ini, attaching any forwarded `PLATFORMIO_*` env
        // var overrides from the CLI caller (the daemon does not inherit
        // caller env vars).
        let ini_path = project_dir.join("platformio.ini");
        let pio_overrides = fbuild_config::PioEnvOverrides::from_map(params.pio_env.clone());
        let config =
            fbuild_config::PlatformIOConfig::from_path_with_overrides(&ini_path, pio_overrides)?;
        let env_config = config.get_env_config(env_name)?;
        let overlay =
            crate::script_runtime::resolve_extra_script_overlay(project_dir, env_name, &config)?;

        // 2. Load board config
        let board_id = env_config.get("board").ok_or_else(|| {
            fbuild_core::FbuildError::ConfigError("missing 'board' in environment config".into())
        })?;
        let overrides = config.get_board_overrides(env_name)?;
        let board = fbuild_config::BoardConfig::from_board_id(board_id, &overrides)?;

        // 3. Build log initialization
        let mut build_log = if params.no_timestamp {
            crate::build_output::create_build_log(params.log_sender.clone())
        } else {
            crate::build_output::create_build_log_with_epoch(
                params.log_sender.clone(),
                std::time::Instant::now(),
            )
        };
        crate::build_output::log_build_banner(&mut build_log, env_name);
        crate::build_output::log_board_info(
            &mut build_log,
            &board.name,
            &board.mcu,
            &board.f_cpu,
            board.max_flash,
            board.max_ram,
        );
        for note in &overlay.notes {
            build_log.push(format!("extra_scripts: {}", note));
        }

        // 4. Setup build directories
        let cache = fbuild_packages::Cache::new(project_dir);
        if params.clean {
            cache.clean_build(env_name, params.profile)?;
        }
        cache.ensure_build_directories(env_name, params.profile)?;

        let build_dir = cache.get_build_dir(env_name, params.profile);
        let core_build_dir = cache.get_core_build_dir(env_name, params.profile);
        let src_build_dir = cache.get_src_build_dir(env_name, params.profile);

        // 5. Resolve source directory (Arduino IDE convention: fall back to project root)
        // Priority: explicit override (from HTTP request) > env var > INI config > "src"
        let src_dir = project_dir.join(
            params
                .src_dir
                .as_deref()
                .map(|s| s.to_string())
                .or_else(|| config.get_src_dir(env_name).ok().flatten())
                .unwrap_or_else(|| "src".to_string()),
        );
        let src_dir = if src_dir.exists() {
            src_dir
        } else {
            project_dir.to_path_buf()
        };
        let source_filter = config.get_source_filter(env_name)?;

        // 6. Collect user flags
        let build_type = config.get_build_type(env_name)?;
        let user_flags = config.get_build_flags(env_name)?;
        crate::warn_debug_build_flags(&user_flags);
        let src_flags = config.get_build_src_flags(env_name)?;
        let overlay_link_flags = overlay.link.flags.clone();
        let (user_flags, src_flags, mut overlay_link_flags) = if build_type == "debug" {
            let debug_build_flags = config.get_debug_build_flags(env_name)?;
            apply_debug_build_type(
                user_flags,
                src_flags,
                overlay_link_flags,
                &debug_build_flags,
            )
        } else {
            (user_flags, src_flags, overlay_link_flags)
        };
        let build_unflags = config.get_build_unflags(env_name)?;
        let (user_flags, src_flags, all_src_flags) =
            apply_build_unflags(user_flags, src_flags, &build_unflags);
        remove_unflagged_tokens(&mut overlay_link_flags, &build_unflags);

        Ok(Self {
            config,
            board,
            build_log,
            build_dir,
            core_build_dir,
            src_build_dir,
            src_dir,
            source_filter,
            user_flags,
            src_flags,
            all_src_flags,
            global_compile_overlay: overlay.global_compile,
            project_compile_overlay: overlay.project_compile,
            overlay_link_flags,
            overlay_link_libs: overlay.link.libs,
        })
    }
}

fn apply_build_unflags(
    mut user_flags: Vec<String>,
    mut src_flags: Vec<String>,
    build_unflags: &[String],
) -> (Vec<String>, Vec<String>, Vec<String>) {
    if build_unflags.is_empty() {
        let all_src_flags = user_flags.iter().chain(src_flags.iter()).cloned().collect();
        return (user_flags, src_flags, all_src_flags);
    }

    remove_unflagged_tokens(&mut user_flags, build_unflags);
    remove_unflagged_tokens(&mut src_flags, build_unflags);
    let all_src_flags = user_flags.iter().chain(src_flags.iter()).cloned().collect();
    (user_flags, src_flags, all_src_flags)
}

fn apply_debug_build_type(
    mut user_flags: Vec<String>,
    mut src_flags: Vec<String>,
    mut link_flags: Vec<String>,
    debug_build_flags: &[String],
) -> (Vec<String>, Vec<String>, Vec<String>) {
    cleanup_platformio_debug_scope(&mut user_flags);
    cleanup_platformio_debug_scope(&mut src_flags);
    cleanup_platformio_debug_scope(&mut link_flags);

    let mut compile_debug_flags = vec!["-D__PLATFORMIO_BUILD_DEBUG__".to_string()];
    compile_debug_flags.extend(debug_build_flags.iter().cloned());

    user_flags.extend(compile_debug_flags.iter().cloned());
    src_flags.extend(compile_debug_flags);

    let link_debug_flags: Vec<String> = debug_build_flags
        .iter()
        .filter(|flag| is_platformio_debug_link_flag(flag))
        .cloned()
        .collect();
    link_flags.extend(link_debug_flags);

    (user_flags, src_flags, link_flags)
}

fn remove_unflagged_tokens(flags: &mut Vec<String>, build_unflags: &[String]) {
    let mut i = 0;
    while i < build_unflags.len() {
        let token = &build_unflags[i];
        if flag_takes_value(token) && i + 1 < build_unflags.len() {
            remove_flag_value_pair(flags, token, &build_unflags[i + 1]);
            i += 2;
        } else {
            flags.retain(|flag| flag != token);
            i += 1;
        }
    }
}

fn remove_flag_value_pair(flags: &mut Vec<String>, option: &str, value: &str) {
    let mut filtered = Vec::with_capacity(flags.len());
    let mut i = 0;
    while i < flags.len() {
        let current = &flags[i];
        if current == option && i + 1 < flags.len() && flags[i + 1] == value {
            i += 2;
            continue;
        }
        filtered.push(current.clone());
        i += 1;
    }
    *flags = filtered;
}

fn cleanup_platformio_debug_scope(flags: &mut Vec<String>) {
    flags.retain(|flag| !is_platformio_debug_cleanup_flag(flag));
}

fn is_platformio_debug_cleanup_flag(flag: &str) -> bool {
    if flag == "-Os" || flag == "-g" {
        return true;
    }
    if flag.len() == 3 {
        let bytes = flag.as_bytes();
        if bytes[0] == b'-' && matches!(bytes[2], b'0' | b'1' | b'2' | b'3') {
            return matches!(bytes[1], b'O' | b'g');
        }
    }
    if flag.len() == 6 && flag.starts_with("-ggdb") {
        return matches!(flag.as_bytes()[5], b'0' | b'1' | b'2' | b'3');
    }
    false
}

fn is_platformio_debug_link_flag(flag: &str) -> bool {
    flag.starts_with("-O") || flag == "-g" || flag.starts_with("-g")
}

fn flag_takes_value(flag: &str) -> bool {
    matches!(
        flag,
        "-include"
            | "-imacros"
            | "-isystem"
            | "-iquote"
            | "-iprefix"
            | "-iwithprefix"
            | "-iwithprefixbefore"
            | "-Xlinker"
            | "-Wa"
            | "-Wl"
            | "-Wp"
            | "-L"
            | "-T"
    )
}

/// Add the project's `include/` directory and `lib/` subdirectories to include paths.
///
/// PlatformIO automatically adds these — replicate that behavior.
pub fn discover_project_includes(project_dir: &Path, include_dirs: &mut Vec<PathBuf>) {
    // PlatformIO automatically includes the project's include/ directory
    let include_dir = project_dir.join("include");
    if include_dir.is_dir() {
        include_dirs.push(include_dir);
    }

    // PlatformIO automatically discovers libraries placed in the project's lib/ directory.
    // Each subdirectory is treated as a library — add its root (and src/ if present).
    let local_lib_dir = project_dir.join("lib");
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

    // Project-as-library detection (PlatformIO convention).
    // When a project root contains library.json or library.properties, the project
    // itself is a library and its src/ directory is automatically added to include
    // paths for any sketch built within the project. This allows building example
    // sketches against the library being developed (e.g., FastLED examples).
    let library_json = project_dir.join("library.json");
    let library_props = project_dir.join("library.properties");
    if library_json.exists() || library_props.exists() {
        let project_src = project_dir.join("src");
        if project_src.is_dir() && !include_dirs.contains(&project_src) {
            include_dirs.push(project_src);
        }
    }
}

/// Returns true if the project is a PlatformIO library (has library.json or library.properties).
pub fn is_project_a_library(project_dir: &Path) -> bool {
    project_dir.join("library.json").exists() || project_dir.join("library.properties").exists()
}

/// Compile a list of sources in parallel with incremental rebuild detection.
///
/// Thin wrapper over [`crate::parallel::compile_sources_parallel`] that flushes
/// collected warnings into the shared build log Mutex. Used by
/// [`run_sequential_build_with_libs`]; ESP32 calls `compile_sources_parallel`
/// directly because it interleaves multiple compile phases through the same
/// log Mutex.
pub fn compile_sources(
    compiler: &dyn Compiler,
    sources: &[PathBuf],
    build_dir: &Path,
    extra_flags: &LanguageExtraFlags,
    jobs: usize,
    build_log: &std::sync::Mutex<BuildLog>,
) -> Result<Vec<PathBuf>> {
    let result = crate::parallel::compile_sources_parallel(
        compiler,
        sources,
        build_dir,
        extra_flags,
        jobs,
        Some(build_log),
    )?;
    if !result.warnings.is_empty() {
        let mut log = build_log.lock().unwrap();
        for w in &result.warnings {
            crate::build_output::collect_warnings(w, &mut log);
        }
    }
    Ok(result.objects)
}

/// Compile all libraries in the project's `lib/` directory.
///
/// Each library's source files are compiled in parallel via
/// [`crate::parallel::compile_sources_parallel`]. Libraries themselves are
/// processed one after another so the per-lib `jobs` budget isn't oversubscribed.
pub fn compile_local_libraries(
    compiler: &dyn Compiler,
    project_dir: &Path,
    build_dir: &Path,
    extra_flags: &LanguageExtraFlags,
    jobs: usize,
    build_log: &std::sync::Mutex<BuildLog>,
) -> Result<Vec<PathBuf>> {
    let mut library_objects = Vec::new();
    let local_lib_dir = project_dir.join("lib");
    if !local_lib_dir.is_dir() {
        return Ok(library_objects);
    }
    let entries = match std::fs::read_dir(&local_lib_dir) {
        Ok(e) => e,
        Err(_) => return Ok(library_objects),
    };
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
            fbuild_packages::library::library_info::InstalledLibrary::new(&lib_path, &lib_name);
        let lib_sources = lib_info.get_source_files();
        if lib_sources.is_empty() {
            continue;
        }

        let lib_build_dir = build_dir.join("lib").join(&lib_name);
        std::fs::create_dir_all(&lib_build_dir)?;
        tracing::info!(
            "compiling local library '{}': {} source files",
            lib_name,
            lib_sources.len()
        );

        let result = crate::parallel::compile_sources_parallel(
            compiler,
            &lib_sources,
            &lib_build_dir,
            extra_flags,
            jobs,
            Some(build_log),
        )
        .map_err(|e| {
            fbuild_core::FbuildError::BuildFailed(format!(
                "local library '{}' compilation failed: {}",
                lib_name, e
            ))
        })?;
        library_objects.extend(result.objects);
        if !result.warnings.is_empty() {
            let mut log = build_log.lock().unwrap();
            for w in &result.warnings {
                crate::build_output::collect_warnings(w, &mut log);
            }
        }
    }
    Ok(library_objects)
}

/// Generate `compile_commands.json` from core/variant and sketch sources.
#[allow(clippy::too_many_arguments)]
pub fn generate_compile_db(
    gcc_path: &Path,
    gxx_path: &Path,
    c_flags: &[String],
    cpp_flags: &[String],
    include_flags: &[String],
    user_flags: &LanguageExtraFlags,
    all_src_flags: &LanguageExtraFlags,
    core_sources: &[PathBuf],
    sketch_sources: &[PathBuf],
    core_build_dir: &Path,
    src_build_dir: &Path,
    build_dir: &Path,
    project_dir: &Path,
    arch: TargetArchitecture,
) -> Result<Option<PathBuf>> {
    let mut compile_db = CompileDatabase::new();
    compile_db.extend(compile_database::generate_entries(
        gcc_path,
        gxx_path,
        c_flags,
        cpp_flags,
        include_flags,
        user_flags,
        core_sources,
        core_build_dir,
        project_dir,
    ));
    compile_db.extend(compile_database::generate_entries(
        gcc_path,
        gxx_path,
        c_flags,
        cpp_flags,
        include_flags,
        all_src_flags,
        sketch_sources,
        src_build_dir,
        project_dir,
    ));
    let compile_db = compile_db.translate_for_clang(arch);
    if compile_db.has_entries() {
        Ok(Some(compile_db.write_and_copy(build_dir, project_dir)?))
    } else {
        Ok(None)
    }
}

/// Log size report and artifacts from a link result.
///
/// When `symbol_analysis_path` is `Some`, the report is written to that path
/// and only a one-liner is logged (unless `verbose` is true, which also streams
/// the full report). When `None`, the report is written to `symbol_analysis.txt`
/// in the build artifacts directory and streamed to the build log.
pub fn handle_link_result(
    link_result: &LinkResult,
    build_log: &mut BuildLog,
    symbol_analysis_path: Option<&Path>,
    verbose: bool,
) {
    if link_result.hex_path.is_some() {
        crate::build_output::log_linking(build_log, "Building firmware.hex");
    } else if link_result.bin_path.is_some() {
        crate::build_output::log_linking(build_log, "Building firmware.bin");
    }

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
        crate::build_output::log_size_report(build_log, size);
    }

    if let Some(ref symbols) = link_result.symbol_map {
        let report = crate::build_output::format_symbol_report(symbols);

        if let Some(path) = symbol_analysis_path {
            // User gave an explicit path — write there, log a one-liner
            if let Err(e) = std::fs::write(path, &report) {
                tracing::warn!("failed to write symbol analysis: {e}");
            } else {
                build_log.push(format!("Symbol analysis written to {}", path.display()));
            }
            // Also stream full report when --verbose
            if verbose {
                crate::build_output::log_symbol_report(build_log, symbols);
            }
        } else {
            // No path — stream to console and write to artifacts dir
            crate::build_output::log_symbol_report(build_log, symbols);
            if let Some(ref elf) = link_result.elf_path {
                if let Some(build_dir) = elf.parent() {
                    let txt_path = build_dir.join("symbol_analysis.txt");
                    if let Err(e) = std::fs::write(&txt_path, &report) {
                        tracing::warn!("failed to write symbol_analysis.txt: {e}");
                    } else {
                        build_log.push(format!("Symbol analysis: {}", txt_path.display()));
                    }
                }
            }
        }
    }

    if let Some(ref elf) = link_result.elf_path {
        crate::build_output::log_artifact(build_log, elf);
    }
    let firmware = link_result
        .hex_path
        .as_ref()
        .or(link_result.bin_path.as_ref());
    if let Some(fw) = firmware {
        crate::build_output::log_artifact(build_log, fw);
    }
}

/// Assemble the final `BuildResult` from link output and build metadata.
pub fn assemble_build_result(
    link_result: LinkResult,
    elapsed: f64,
    platform_label: &str,
    env_name: &str,
    compile_database_path: Option<PathBuf>,
    build_log: BuildLog,
) -> BuildResult {
    tracing::info!("build completed in {:.1}s", elapsed);
    BuildResult {
        success: true,
        firmware_path: link_result.bin_path.or(link_result.hex_path),
        elf_path: link_result.elf_path,
        size_info: link_result.size_info,
        symbol_map: link_result.symbol_map,
        build_time_secs: elapsed,
        message: format!("{} build for {} completed", platform_label, env_name),
        compile_database_path,
        build_log,
    }
}

/// Run the sequential compile → link → result pipeline used by AVR, Teensy,
/// RP2040, STM32, ESP8266, CH32V, NRF52, SAM, Renesas, and Apollo3.
///
/// Handles: compiledb_only early return, sequential compilation of
/// core/variant/sketch/libs, compile database generation, linking, and result
/// assembly.
///
/// ESP32 cannot use this because it uses parallel compilation and has
/// additional hooks (SDK libs, embed files, bootloader prep). It calls
/// [`compile_project_as_library`] directly.
///
/// When `lib_env` is `Some`, the project's own `src/` is compiled as a library
/// archive (matching PlatformIO's project-as-library convention) and linked
/// with the rest of the build. See [`compile_project_as_library`] and
/// ISSUES.md Issue 1.
#[allow(clippy::too_many_arguments)]
pub fn run_sequential_build_with_libs(
    compiler: &dyn Compiler,
    linker: &dyn crate::linker::Linker,
    mut ctx: BuildContext,
    params: &BuildParams,
    sources: &SourceCollection,
    extra_link_inputs: &[PathBuf],
    lib_env: Option<&LibraryBuildEnv<'_>>,
    arch: TargetArchitecture,
    platform_label: &str,
    start: Instant,
) -> Result<BuildResult> {
    let core_and_variant: Vec<PathBuf> = sources
        .core_sources
        .iter()
        .chain(sources.variant_sources.iter())
        .cloned()
        .collect();
    let user_overlay = LanguageExtraFlags {
        common: ctx
            .user_flags
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

    // compiledb_only: generate compile_commands.json without compiling
    if params.compiledb_only {
        let compile_database_path = generate_compile_db(
            compiler.gcc_path(),
            compiler.gxx_path(),
            &compiler.c_flags(),
            &compiler.cpp_flags(),
            &[],
            &user_overlay,
            &src_overlay,
            &core_and_variant,
            &sources.sketch_sources,
            &ctx.core_build_dir,
            &ctx.src_build_dir,
            &ctx.build_dir,
            &params.project_dir,
            arch,
        )?;
        let elapsed = start.elapsed().as_secs_f64();
        return Ok(BuildResult {
            success: true,
            firmware_path: None,
            elf_path: None,
            size_info: None,
            symbol_map: None,
            build_time_secs: elapsed,
            message: format!("compile_commands.json generated for {}", params.env_name),
            compile_database_path,
            build_log: ctx.build_log,
        });
    }

    // Wrap the build log so it can be shared across parallel compile phases.
    // Phases still run one after another (compile core → variant → sketch →
    // libs → link), but each phase fans out file compilation across `jobs`
    // threads via `compile_sources_parallel`.
    let jobs = crate::parallel::effective_jobs(params.jobs);
    let build_log_mutex = std::sync::Mutex::new(ctx.build_log);

    // Compile core + variant
    let mut core_objects = compile_sources(
        compiler,
        &sources.core_sources,
        &ctx.core_build_dir,
        &user_overlay,
        jobs,
        &build_log_mutex,
    )?;
    let variant_objects = compile_sources(
        compiler,
        &sources.variant_sources,
        &ctx.core_build_dir,
        &user_overlay,
        jobs,
        &build_log_mutex,
    )?;
    core_objects.extend(variant_objects);

    // Compile sketch
    let sketch_objects = compile_sources(
        compiler,
        &sources.sketch_sources,
        &ctx.src_build_dir,
        &src_overlay,
        jobs,
        &build_log_mutex,
    )?;

    // Compile local libraries (lib/* — loose objects, LTO-safe; per-lib parallel)
    let library_objects = compile_local_libraries(
        compiler,
        &params.project_dir,
        &ctx.build_dir,
        &src_overlay,
        jobs,
        &build_log_mutex,
    )?;

    // Unwrap the build log Mutex back into the context for the remaining
    // single-threaded phases (link, result assembly).
    ctx.build_log = build_log_mutex.into_inner().unwrap();

    // Project-as-library: compile project root's src/ as an archive when
    // building an example sketch from a library project (e.g. FastLED examples).
    // Only runs when caller provided a LibraryBuildEnv with toolchain paths.
    let project_as_lib_archive: Option<PathBuf> = if let Some(env) = lib_env {
        // Collect existing lib/* names so the helper can detect collisions.
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
        compile_project_as_library(
            &params.project_dir,
            &ctx.src_dir,
            &ctx.build_dir,
            env,
            &existing_lib_names,
        )?
    } else {
        None
    };

    // Generate compile_commands.json
    let compile_database_path = generate_compile_db(
        compiler.gcc_path(),
        compiler.gxx_path(),
        &compiler.c_flags(),
        &compiler.cpp_flags(),
        &[],
        &user_overlay,
        &src_overlay,
        &core_and_variant,
        &sources.sketch_sources,
        &ctx.core_build_dir,
        &ctx.src_build_dir,
        &ctx.build_dir,
        &params.project_dir,
        arch,
    )?;

    // Link
    crate::build_output::log_linking(&mut ctx.build_log, "Linking firmware.elf");
    core_objects.extend(library_objects);
    core_objects.extend(extra_link_inputs.iter().cloned());
    if let Some(archive) = project_as_lib_archive {
        // GCC accepts .a in the same positional slot as .o files.
        core_objects.push(archive);
    }
    let link_result = crate::linker::Linker::link_all(
        linker,
        &sketch_objects,
        &core_objects,
        &ctx.build_dir,
        &crate::linker::LinkExtraArgs {
            flags: ctx.overlay_link_flags.clone(),
            libs: ctx.overlay_link_libs.clone(),
        },
        params.symbol_analysis,
    )?;

    // Result
    handle_link_result(
        &link_result,
        &mut ctx.build_log,
        params.symbol_analysis_path.as_deref(),
        params.verbose,
    );
    let elapsed = start.elapsed().as_secs_f64();
    Ok(assemble_build_result(
        link_result,
        elapsed,
        platform_label,
        &params.env_name,
        compile_database_path,
        ctx.build_log,
    ))
}

/// Log the version of a GCC toolchain by running `gcc -dumpversion`.
pub fn log_toolchain_version(gcc_path: &Path, label: &str, build_log: &mut BuildLog) {
    if let Ok(ver_out) = fbuild_core::subprocess::run_command(
        &[gcc_path.to_string_lossy().as_ref(), "-dumpversion"],
        None,
        None,
        None,
    ) {
        let version = ver_out.stdout.trim().to_string();
        if !version.is_empty() {
            crate::build_output::log_toolchain_version(build_log, label, &version);
        }
    }
}

/// Tool paths and flag sets needed to compile and archive a standalone library.
///
/// Bundles parameters that flow together through library-compilation helpers
/// (replaces several `#[allow(clippy::too_many_arguments)]` sites).
#[derive(Debug, Clone)]
pub struct LibraryBuildEnv<'a> {
    pub gcc_path: &'a Path,
    pub gxx_path: &'a Path,
    /// Archiver path. For LTO-enabled builds, callers should pass the
    /// toolchain's `gcc-ar` (`Toolchain::get_gcc_ar_path()`) so the
    /// linker-plugin index gets written into the archive. See ISSUES.md
    /// Issue 8.
    pub ar_path: &'a Path,
    pub c_flags: &'a [String],
    pub cpp_flags: &'a [String],
    pub include_dirs: &'a [PathBuf],
    pub verbose: bool,
    pub jobs: usize,
    pub compiler_cache: Option<&'a Path>,
}

/// Pick the LTO-aware archiver when any compile flag enables LTO.
///
/// Plain `ar` doesn't insert the LTO linker-plugin index, so on toolchains
/// where the plugin path isn't auto-discovered, the linker silently drops
/// LTO-only symbols. The `gcc-ar` wrapper writes the index — use it whenever
/// `-flto` (or `-flto=auto`) is in the compile flags.
///
/// `gcc_ar_path` should come from `Toolchain::get_gcc_ar_path()`, which
/// already falls back to `ar` when `gcc-ar` isn't available on disk.
pub fn pick_archiver<'a>(
    ar_path: &'a Path,
    gcc_ar_path: &'a Path,
    c_flags: &[String],
    cpp_flags: &[String],
) -> &'a Path {
    let has_lto = c_flags.iter().any(|f| f.starts_with("-flto"))
        || cpp_flags.iter().any(|f| f.starts_with("-flto"));
    if has_lto {
        gcc_ar_path
    } else {
        ar_path
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_apply_build_unflags_removes_exact_tokens_from_global_and_src_flags() {
        let (user_flags, src_flags, all_src_flags) = super::apply_build_unflags(
            vec![
                "-Os".to_string(),
                "-DDEBUG".to_string(),
                "-Wall".to_string(),
            ],
            vec!["-DDEBUG".to_string(), "-Winvalid-pch".to_string()],
            &["-DDEBUG".to_string(), "-Os".to_string()],
        );

        assert_eq!(user_flags, vec!["-Wall"]);
        assert_eq!(src_flags, vec!["-Winvalid-pch"]);
        assert_eq!(all_src_flags, vec!["-Wall", "-Winvalid-pch"]);
    }

    #[test]
    fn test_apply_build_unflags_removes_common_option_value_pair() {
        let (user_flags, src_flags, all_src_flags) = super::apply_build_unflags(
            vec![
                "-include".to_string(),
                "config/common.h".to_string(),
                "-Wall".to_string(),
            ],
            vec![
                "-include".to_string(),
                "config/common.h".to_string(),
                "-Winvalid-pch".to_string(),
            ],
            &["-include".to_string(), "config/common.h".to_string()],
        );

        assert_eq!(user_flags, vec!["-Wall"]);
        assert_eq!(src_flags, vec!["-Winvalid-pch"]);
        assert_eq!(all_src_flags, vec!["-Wall", "-Winvalid-pch"]);
    }

    #[test]
    fn test_apply_debug_build_type_replaces_opt_flags_and_adds_debug_define() {
        let (user_flags, src_flags, link_flags) = super::apply_debug_build_type(
            vec!["-Os".to_string(), "-Wall".to_string()],
            vec!["-O2".to_string(), "-Winvalid-pch".to_string()],
            Vec::new(),
            &["-Og".to_string(), "-g2".to_string(), "-ggdb2".to_string()],
        );

        assert_eq!(
            user_flags,
            vec![
                "-Wall",
                "-D__PLATFORMIO_BUILD_DEBUG__",
                "-Og",
                "-g2",
                "-ggdb2"
            ]
        );
        assert_eq!(
            src_flags,
            vec![
                "-Winvalid-pch",
                "-D__PLATFORMIO_BUILD_DEBUG__",
                "-Og",
                "-g2",
                "-ggdb2"
            ]
        );
        assert_eq!(link_flags, vec!["-Og", "-g2", "-ggdb2"]);
    }

    #[test]
    fn test_debug_mode_then_build_unflags_can_remove_debug_flags_again() {
        let (user_flags, src_flags, mut link_flags) = super::apply_debug_build_type(
            vec!["-Os".to_string(), "-Wall".to_string()],
            vec!["-Winvalid-pch".to_string()],
            Vec::new(),
            &["-Og".to_string(), "-g2".to_string(), "-ggdb2".to_string()],
        );
        let (user_flags, src_flags, all_src_flags) =
            super::apply_build_unflags(user_flags, src_flags, &["-g2".to_string()]);
        super::remove_unflagged_tokens(&mut link_flags, &["-g2".to_string()]);

        assert_eq!(
            user_flags,
            vec!["-Wall", "-D__PLATFORMIO_BUILD_DEBUG__", "-Og", "-ggdb2"]
        );
        assert_eq!(
            src_flags,
            vec![
                "-Winvalid-pch",
                "-D__PLATFORMIO_BUILD_DEBUG__",
                "-Og",
                "-ggdb2"
            ]
        );
        assert_eq!(
            all_src_flags,
            vec![
                "-Wall",
                "-D__PLATFORMIO_BUILD_DEBUG__",
                "-Og",
                "-ggdb2",
                "-Winvalid-pch",
                "-D__PLATFORMIO_BUILD_DEBUG__",
                "-Og",
                "-ggdb2"
            ]
        );
        assert_eq!(link_flags, vec!["-Og", "-ggdb2"]);
    }
}

/// Compile the project's own `src/` as a library archive, when the project
/// root contains `library.json`/`library.properties` and we're building an
/// example sketch (i.e. `src_dir` points elsewhere).
///
/// Returns `Ok(None)` when not applicable (not a library project, normal
/// build, header-only, no src dir, or name collides with a `lib/`
/// subdirectory). Returns `Ok(Some(archive_path))` when the project-as-
/// library archive was produced.
///
/// Matches PlatformIO's project-as-library convention; see ISSUES.md Issue 1.
pub fn compile_project_as_library(
    project_dir: &Path,
    src_dir: &Path,
    build_dir: &Path,
    env: &LibraryBuildEnv<'_>,
    existing_lib_names: &std::collections::HashSet<String>,
) -> Result<Option<PathBuf>> {
    // Guard 1: must be a library project (library.json or library.properties at root).
    if !is_project_a_library(project_dir) {
        return Ok(None);
    }

    // Guard 2: project must have a src/ dir.
    let project_src = project_dir.join("src");
    if !project_src.is_dir() {
        return Ok(None);
    }

    // Guard 3: must be building an example. If src_dir IS the project's own
    // src/, we're doing a normal library self-build and the sketch scanner
    // is already compiling these sources — don't double-compile.
    // Also guard the BuildContext fallback where src_dir collapses to
    // project_dir (would cause the scanner to recursively pick up library
    // sources, leading to multiply-defined symbols).
    if src_dir == project_src || src_dir == project_dir {
        return Ok(None);
    }

    // Compute lib name from project dir basename.
    let lib_name = project_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("project")
        .to_lowercase();

    // Guard 4: collision with a lib/<name>/ subdirectory — lib/ wins
    // (matches PlatformIO behavior).
    if existing_lib_names.contains(&lib_name) {
        tracing::warn!(
            "project-as-library '{}' collides with lib/{} — skipping project root",
            lib_name,
            lib_name
        );
        return Ok(None);
    }

    // Discover sources via the same helper used for installed libraries.
    let lib_info =
        fbuild_packages::library::library_info::InstalledLibrary::new(project_dir, &lib_name);
    let sources = lib_info.get_source_files();
    if sources.is_empty() {
        tracing::info!("project-as-library '{}' is header-only", lib_name);
        return Ok(None);
    }

    tracing::info!(
        "compiling project-as-library: {} ({} sources from {})",
        lib_name,
        sources.len(),
        project_src.display()
    );

    let project_libs_dir = build_dir.join("project_lib");
    std::fs::create_dir_all(&project_libs_dir)?;

    match fbuild_packages::library::library_compiler::compile_library_with_jobs(
        &lib_name,
        &sources,
        env.include_dirs,
        env.gcc_path,
        env.gxx_path,
        env.ar_path,
        env.c_flags,
        env.cpp_flags,
        &project_libs_dir,
        env.verbose,
        env.jobs,
        env.compiler_cache,
    ) {
        Ok(Some(archive)) => {
            tracing::info!(
                "project-as-library compiled: {} sources -> {}",
                sources.len(),
                archive.display()
            );
            Ok(Some(archive))
        }
        Ok(None) => Ok(None), // unreachable when sources is non-empty, but safe
        Err(e) => Err(fbuild_core::FbuildError::BuildFailed(format!(
            "project-as-library '{}' compilation failed: {}",
            lib_name, e
        ))),
    }
}

/// Check if a project is configured for a specific platform by reading its platformio.ini.
pub fn is_platform_project(
    project_dir: &Path,
    env_name: &str,
    platform: fbuild_core::Platform,
) -> bool {
    let ini_path = project_dir.join("platformio.ini");
    if let Ok(config) = fbuild_config::PlatformIOConfig::from_path(&ini_path) {
        if let Ok(env) = config.get_env_config(env_name) {
            if let Some(platform_str) = env.get("platform") {
                return platform.matches_str(platform_str);
            }
        }
    }
    false
}

#[cfg(test)]
mod pick_archiver_tests {
    use super::*;

    #[test]
    fn test_picks_plain_ar_without_lto() {
        let ar = Path::new("/tc/bin/avr-ar");
        let gcc_ar = Path::new("/tc/bin/avr-gcc-ar");
        let c_flags = vec!["-Os".to_string()];
        let cpp_flags = vec!["-std=gnu++17".to_string()];
        assert_eq!(pick_archiver(ar, gcc_ar, &c_flags, &cpp_flags), ar);
    }

    #[test]
    fn test_picks_gcc_ar_when_c_flags_have_lto() {
        let ar = Path::new("/tc/bin/avr-ar");
        let gcc_ar = Path::new("/tc/bin/avr-gcc-ar");
        let c_flags = vec!["-Os".to_string(), "-flto".to_string()];
        let cpp_flags: Vec<String> = vec![];
        assert_eq!(pick_archiver(ar, gcc_ar, &c_flags, &cpp_flags), gcc_ar);
    }

    #[test]
    fn test_picks_gcc_ar_when_cpp_flags_have_lto_auto() {
        let ar = Path::new("/tc/bin/xtensa-esp-elf-ar");
        let gcc_ar = Path::new("/tc/bin/xtensa-esp-elf-gcc-ar");
        let c_flags: Vec<String> = vec![];
        let cpp_flags = vec!["-flto=auto".to_string()];
        assert_eq!(pick_archiver(ar, gcc_ar, &c_flags, &cpp_flags), gcc_ar);
    }
}

#[cfg(test)]
mod project_as_library_tests {
    use super::*;
    use std::collections::HashSet;

    /// Helper: build a `LibraryBuildEnv` with bogus tool paths.
    ///
    /// Safe to use whenever the guard logic is expected to short-circuit BEFORE
    /// any tool invocation. If the function actually tries to compile, the
    /// bogus paths force an error so the test fails loudly.
    fn bogus_env<'a>(
        include_dirs: &'a [PathBuf],
        c_flags: &'a [String],
        cpp_flags: &'a [String],
    ) -> LibraryBuildEnv<'a> {
        // Use empty paths so any subprocess invocation would fail fast.
        // We rely on the test's tempdir scope to keep references alive.
        LibraryBuildEnv {
            gcc_path: Path::new("/__bogus__/gcc"),
            gxx_path: Path::new("/__bogus__/g++"),
            ar_path: Path::new("/__bogus__/ar"),
            c_flags,
            cpp_flags,
            include_dirs,
            verbose: false,
            jobs: 1,
            compiler_cache: None,
        }
    }

    #[test]
    fn test_returns_none_when_not_a_library() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path();
        // No library.json or library.properties
        std::fs::create_dir_all(project_dir.join("src")).unwrap();
        std::fs::write(project_dir.join("src").join("lib.cpp"), "").unwrap();

        let src_dir = project_dir.join("examples").join("Demo");
        let include_dirs: Vec<PathBuf> = vec![];
        let c_flags: Vec<String> = vec![];
        let cpp_flags: Vec<String> = vec![];
        let env = bogus_env(&include_dirs, &c_flags, &cpp_flags);

        let result = compile_project_as_library(
            project_dir,
            &src_dir,
            &project_dir.join("build"),
            &env,
            &HashSet::new(),
        );
        assert!(matches!(result, Ok(None)));
    }

    #[test]
    fn test_returns_none_when_src_dir_equals_project_src() {
        // Library project being built normally (not as an example) — must
        // NOT compile project-as-library or we'd double-compile sketch sources.
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path();
        std::fs::write(project_dir.join("library.json"), r#"{"name": "test"}"#).unwrap();
        let project_src = project_dir.join("src");
        std::fs::create_dir_all(&project_src).unwrap();
        std::fs::write(project_src.join("lib.cpp"), "").unwrap();

        let include_dirs: Vec<PathBuf> = vec![];
        let c_flags: Vec<String> = vec![];
        let cpp_flags: Vec<String> = vec![];
        let env = bogus_env(&include_dirs, &c_flags, &cpp_flags);

        let result = compile_project_as_library(
            project_dir,
            &project_src,
            &project_dir.join("build"),
            &env,
            &HashSet::new(),
        );
        assert!(matches!(result, Ok(None)));
    }

    #[test]
    fn test_returns_none_when_src_dir_equals_project_dir() {
        // BuildContext::new falls back to project_dir when the resolved src
        // dir doesn't exist. In that fallback, the sketch scanner walks
        // project_dir recursively and would pick up library sources — so we
        // must skip project-as-library to avoid multiply-defined symbols.
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path();
        std::fs::write(project_dir.join("library.json"), r#"{"name": "test"}"#).unwrap();
        std::fs::create_dir_all(project_dir.join("src")).unwrap();
        std::fs::write(project_dir.join("src").join("lib.cpp"), "").unwrap();

        let include_dirs: Vec<PathBuf> = vec![];
        let c_flags: Vec<String> = vec![];
        let cpp_flags: Vec<String> = vec![];
        let env = bogus_env(&include_dirs, &c_flags, &cpp_flags);

        let result = compile_project_as_library(
            project_dir,
            project_dir, // src_dir == project_dir (fallback case)
            &project_dir.join("build"),
            &env,
            &HashSet::new(),
        );
        assert!(matches!(result, Ok(None)));
    }

    #[test]
    fn test_returns_none_when_no_src_dir() {
        // library.properties exists but no src/ directory.
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path();
        std::fs::write(project_dir.join("library.properties"), "name=Test\n").unwrap();

        let src_dir = project_dir.join("examples").join("Demo");
        let include_dirs: Vec<PathBuf> = vec![];
        let c_flags: Vec<String> = vec![];
        let cpp_flags: Vec<String> = vec![];
        let env = bogus_env(&include_dirs, &c_flags, &cpp_flags);

        let result = compile_project_as_library(
            project_dir,
            &src_dir,
            &project_dir.join("build"),
            &env,
            &HashSet::new(),
        );
        assert!(matches!(result, Ok(None)));
    }

    #[test]
    fn test_returns_none_when_header_only() {
        // library.json + src/ but only headers — header-only library, not
        // an error, just nothing to compile.
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path();
        std::fs::write(project_dir.join("library.json"), r#"{"name": "test"}"#).unwrap();
        let project_src = project_dir.join("src");
        std::fs::create_dir_all(&project_src).unwrap();
        std::fs::write(project_src.join("lib.h"), "").unwrap();
        std::fs::write(project_src.join("inline.hpp"), "").unwrap();

        let src_dir = project_dir.join("examples").join("Demo");
        let include_dirs: Vec<PathBuf> = vec![];
        let c_flags: Vec<String> = vec![];
        let cpp_flags: Vec<String> = vec![];
        let env = bogus_env(&include_dirs, &c_flags, &cpp_flags);

        let result = compile_project_as_library(
            project_dir,
            &src_dir,
            &project_dir.join("build"),
            &env,
            &HashSet::new(),
        );
        assert!(matches!(result, Ok(None)));
    }

    #[test]
    fn test_returns_none_on_collision_with_lib_dir() {
        // If a user has both library.json AND lib/<projectname>/, the lib/
        // version wins (matches PlatformIO behavior). Must skip project-as-
        // library to prevent two libfastled.a archives at link time.
        let tmp = tempfile::TempDir::new().unwrap();
        // Create a project dir with a known basename to control lib_name.
        let project_dir = tmp.path().join("FastLED");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(project_dir.join("library.json"), r#"{"name": "FastLED"}"#).unwrap();
        let project_src = project_dir.join("src");
        std::fs::create_dir_all(&project_src).unwrap();
        std::fs::write(project_src.join("FastLED.cpp"), "").unwrap();

        let src_dir = project_dir.join("examples").join("Blink");

        let include_dirs: Vec<PathBuf> = vec![];
        let c_flags: Vec<String> = vec![];
        let cpp_flags: Vec<String> = vec![];
        let env = bogus_env(&include_dirs, &c_flags, &cpp_flags);

        let mut existing = HashSet::new();
        existing.insert("fastled".to_string()); // lowercased project basename

        let result = compile_project_as_library(
            &project_dir,
            &src_dir,
            &project_dir.join("build"),
            &env,
            &existing,
        );
        assert!(matches!(result, Ok(None)));
    }

    #[test]
    fn test_attempts_compile_when_building_example() {
        // The positive case: library project + sketch lives elsewhere + has
        // sources + no name collision → must reach the compile path. We
        // verify this by passing a bogus gcc path and asserting the function
        // ERRORS (not Ok(None)). An Ok(None) here would mean a guard
        // incorrectly skipped the compile.
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path().join("FastLED");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(project_dir.join("library.json"), r#"{"name": "FastLED"}"#).unwrap();
        let project_src = project_dir.join("src");
        std::fs::create_dir_all(&project_src).unwrap();
        std::fs::write(project_src.join("FastLED.cpp"), "// stub").unwrap();

        let src_dir = project_dir.join("examples").join("Blink");
        std::fs::create_dir_all(&src_dir).unwrap();

        let include_dirs: Vec<PathBuf> = vec![];
        let c_flags: Vec<String> = vec![];
        let cpp_flags: Vec<String> = vec![];
        let env = bogus_env(&include_dirs, &c_flags, &cpp_flags);

        let result = compile_project_as_library(
            &project_dir,
            &src_dir,
            &project_dir.join("build"),
            &env,
            &HashSet::new(),
        );
        // Must NOT be Ok(None) — that would mean a guard skipped compile.
        // Either Err (bogus tool failed) or Ok(Some(_)) (impossible without
        // a real toolchain) is acceptable.
        if let Ok(None) = result {
            panic!("expected compile to be attempted, but a guard returned Ok(None)");
        }
    }
}
