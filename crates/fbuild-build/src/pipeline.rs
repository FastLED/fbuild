//! Shared build pipeline helpers used by all platform orchestrators.
//!
//! Extracts the duplicated config-parse → board-load → build-dir-setup → compile → link
//! sequence that was copy-pasted across AVR, Teensy, and ESP32 orchestrators.

use std::path::{Path, PathBuf};
use std::time::Instant;

use fbuild_core::{BuildLog, BuildProfile, Result};

use crate::compile_database::{self, CompileDatabase, TargetArchitecture};
use crate::compiler::{Compiler, CompilerBase};
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
    pub user_flags: Vec<String>,
    pub src_flags: Vec<String>,
    pub all_src_flags: Vec<String>,
}

impl BuildContext {
    /// Parse platformio.ini, load board config, setup build directories,
    /// resolve source directory, and collect user flags.
    pub fn new(
        project_dir: &Path,
        env_name: &str,
        clean: bool,
        profile: BuildProfile,
        log_sender: Option<std::sync::mpsc::Sender<String>>,
    ) -> Result<Self> {
        // 1. Parse platformio.ini
        let ini_path = project_dir.join("platformio.ini");
        let config = fbuild_config::PlatformIOConfig::from_path(&ini_path)?;
        let env_config = config.get_env_config(env_name)?;

        // 2. Load board config
        let board_id = env_config.get("board").ok_or_else(|| {
            fbuild_core::FbuildError::ConfigError("missing 'board' in environment config".into())
        })?;
        let overrides = config.get_board_overrides(env_name)?;
        let board = fbuild_config::BoardConfig::from_board_id(board_id, &overrides)?;

        // 3. Build log initialization
        let mut build_log = crate::build_output::create_build_log(log_sender);
        crate::build_output::log_build_banner(&mut build_log, env_name);
        crate::build_output::log_board_info(
            &mut build_log,
            &board.name,
            &board.mcu,
            &board.f_cpu,
            board.max_flash,
            board.max_ram,
        );

        // 4. Setup build directories
        let cache = fbuild_packages::Cache::new(project_dir);
        if clean {
            cache.clean_build(env_name, profile)?;
        }
        cache.ensure_build_directories(env_name, profile)?;

        let build_dir = cache.get_build_dir(env_name, profile);
        let core_build_dir = cache.get_core_build_dir(env_name, profile);
        let src_build_dir = cache.get_src_build_dir(env_name, profile);

        // 5. Resolve source directory (Arduino IDE convention: fall back to project root)
        let src_dir = project_dir.join(
            config
                .get_src_dir(env_name)?
                .unwrap_or_else(|| "src".to_string()),
        );
        let src_dir = if src_dir.exists() {
            src_dir
        } else {
            project_dir.to_path_buf()
        };

        // 6. Collect user flags
        let user_flags = config.get_build_flags(env_name)?;
        crate::warn_debug_build_flags(&user_flags);
        let src_flags = config.get_build_src_flags(env_name)?;
        let all_src_flags: Vec<String> =
            user_flags.iter().chain(src_flags.iter()).cloned().collect();

        Ok(Self {
            config,
            board,
            build_log,
            build_dir,
            core_build_dir,
            src_build_dir,
            src_dir,
            user_flags,
            src_flags,
            all_src_flags,
        })
    }
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
}

/// Compile a list of sources sequentially with incremental rebuild detection.
///
/// Used by AVR and Teensy orchestrators. ESP32 uses parallel compilation instead.
pub fn compile_sources_sequential(
    compiler: &dyn Compiler,
    sources: &[PathBuf],
    build_dir: &Path,
    extra_flags: &[String],
    build_log: &mut BuildLog,
) -> Result<Vec<PathBuf>> {
    let mut objects = Vec::new();
    for source in sources {
        let obj = CompilerBase::object_path(source, build_dir);
        if CompilerBase::needs_rebuild(source, &obj) {
            crate::build_output::log_compiling(build_log, &obj);
            let result = compiler.compile(source, &obj, extra_flags)?;
            if !result.success {
                return Err(fbuild_core::FbuildError::BuildFailed(format!(
                    "compilation failed for {}:\n{}",
                    source.display(),
                    result.stderr
                )));
            }
            crate::build_output::collect_warnings(&result.stderr, build_log);
        }
        objects.push(obj);
    }
    Ok(objects)
}

/// Compile all libraries in the project's `lib/` directory sequentially.
///
/// Used by AVR and Teensy orchestrators. ESP32 uses parallel library compilation.
pub fn compile_local_libraries(
    compiler: &dyn Compiler,
    project_dir: &Path,
    build_dir: &Path,
    extra_flags: &[String],
    build_log: &mut BuildLog,
) -> Result<Vec<PathBuf>> {
    let mut library_objects = Vec::new();
    let local_lib_dir = project_dir.join("lib");
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
                std::fs::create_dir_all(&lib_build_dir)?;
                tracing::info!(
                    "compiling local library '{}': {} source files",
                    lib_name,
                    lib_sources.len()
                );

                for source in &lib_sources {
                    let obj = CompilerBase::object_path(source, &lib_build_dir);
                    if CompilerBase::needs_rebuild(source, &obj) {
                        crate::build_output::log_compiling(build_log, &obj);
                        let result = compiler.compile(source, &obj, extra_flags)?;
                        if !result.success {
                            return Err(fbuild_core::FbuildError::BuildFailed(format!(
                                "local library '{}' compilation failed for {}:\n{}",
                                lib_name,
                                source.display(),
                                result.stderr
                            )));
                        }
                        crate::build_output::collect_warnings(&result.stderr, build_log);
                    }
                    library_objects.push(obj);
                }
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
    user_flags: &[String],
    all_src_flags: &[String],
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
pub fn handle_link_result(link_result: &LinkResult, build_log: &mut BuildLog) {
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
        build_time_secs: elapsed,
        message: format!("{} build for {} completed", platform_label, env_name),
        compile_database_path,
        build_log,
    }
}

/// Run the sequential compile → link → result pipeline used by AVR and Teensy.
///
/// Handles: compiledb_only early return, sequential compilation of core/variant/sketch/libs,
/// compile database generation, linking, and result assembly.
///
/// ESP32 cannot use this because it uses parallel compilation and has additional
/// hooks (SDK libs, embed files, bootloader prep).
#[allow(clippy::too_many_arguments)]
pub fn run_sequential_build(
    compiler: &dyn Compiler,
    linker: &dyn crate::linker::Linker,
    mut ctx: BuildContext,
    params: &BuildParams,
    sources: &SourceCollection,
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

    // compiledb_only: generate compile_commands.json without compiling
    if params.compiledb_only {
        let compile_database_path = generate_compile_db(
            compiler.gcc_path(),
            compiler.gxx_path(),
            &compiler.c_flags(),
            &compiler.cpp_flags(),
            &[],
            &ctx.user_flags,
            &ctx.all_src_flags,
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
            build_time_secs: elapsed,
            message: format!("compile_commands.json generated for {}", params.env_name),
            compile_database_path,
            build_log: ctx.build_log,
        });
    }

    // Compile core + variant
    let mut core_objects = compile_sources_sequential(
        compiler,
        &sources.core_sources,
        &ctx.core_build_dir,
        &ctx.user_flags,
        &mut ctx.build_log,
    )?;
    let variant_objects = compile_sources_sequential(
        compiler,
        &sources.variant_sources,
        &ctx.core_build_dir,
        &ctx.user_flags,
        &mut ctx.build_log,
    )?;
    core_objects.extend(variant_objects);

    // Compile sketch
    let sketch_objects = compile_sources_sequential(
        compiler,
        &sources.sketch_sources,
        &ctx.src_build_dir,
        &ctx.all_src_flags,
        &mut ctx.build_log,
    )?;

    // Compile local libraries
    let library_objects = compile_local_libraries(
        compiler,
        &params.project_dir,
        &ctx.build_dir,
        &ctx.all_src_flags,
        &mut ctx.build_log,
    )?;

    // Generate compile_commands.json
    let compile_database_path = generate_compile_db(
        compiler.gcc_path(),
        compiler.gxx_path(),
        &compiler.c_flags(),
        &compiler.cpp_flags(),
        &[],
        &ctx.user_flags,
        &ctx.all_src_flags,
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
    let link_result =
        crate::linker::Linker::link_all(linker, &sketch_objects, &core_objects, &ctx.build_dir)?;

    // Result
    handle_link_result(&link_result, &mut ctx.build_log);
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
