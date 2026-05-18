//! The sequential compile → link → result pipeline used by AVR, Teensy,
//! RP2040, STM32, ESP8266, CH32V, NRF52, SAM, Renesas, and Apollo3.

use std::path::PathBuf;
use std::time::Instant;

use fbuild_core::Result;

use crate::compile_database::TargetArchitecture;
use crate::compiler::Compiler;
use crate::flag_overlay::LanguageExtraFlags;
use crate::source_scanner::SourceCollection;
use crate::{BuildParams, BuildResult};

use super::compile::{compile_local_libraries, compile_sources, generate_compile_db};
use super::context::BuildContext;
use super::library::{compile_project_as_library, LibraryBuildEnv};
use super::link::{assemble_build_result, handle_link_result};

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
    // Env-gated per-phase timer (FBUILD_PERF_LOG=1). Emits summary on drop.
    // Zero-overhead when the env var is unset — phase guards become no-ops.
    let mut perf = crate::perf_log::PerfTimer::new("pipeline");
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
    let mut core_objects = {
        let _g = perf.phase("compile-core");
        compile_sources(
            compiler,
            &sources.core_sources,
            &ctx.core_build_dir,
            &user_overlay,
            jobs,
            &build_log_mutex,
        )?
    };
    let variant_objects = {
        let _g = perf.phase("compile-variant");
        compile_sources(
            compiler,
            &sources.variant_sources,
            &ctx.core_build_dir,
            &user_overlay,
            jobs,
            &build_log_mutex,
        )?
    };
    core_objects.extend(variant_objects);

    // Compile sketch
    let sketch_objects = {
        let _g = perf.phase("compile-sketch");
        compile_sources(
            compiler,
            &sources.sketch_sources,
            &ctx.src_build_dir,
            &src_overlay,
            jobs,
            &build_log_mutex,
        )?
    };

    // Compile local libraries (lib/* — loose objects, LTO-safe; per-lib parallel)
    let library_objects = {
        let _g = perf.phase("compile-local-libs");
        compile_local_libraries(
            compiler,
            &params.project_dir,
            &ctx.build_dir,
            &src_overlay,
            jobs,
            &build_log_mutex,
        )?
    };

    // Unwrap the build log Mutex back into the context for the remaining
    // single-threaded phases (link, result assembly).
    ctx.build_log = build_log_mutex.into_inner().unwrap();

    // Project-as-library: compile project root's src/ as an archive when
    // building an example sketch from a library project (e.g. FastLED examples).
    // Only runs when caller provided a LibraryBuildEnv with toolchain paths.
    let project_as_lib_archive: Option<PathBuf> = {
        let _g = perf.phase("project-as-lib");
        if let Some(env) = lib_env {
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
        }
    };

    // Generate compile_commands.json
    let compile_database_path = {
        let _g = perf.phase("compile-db");
        generate_compile_db(
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
        )?
    };

    // Link
    crate::build_output::log_linking(&mut ctx.build_log, "Linking firmware.elf");
    core_objects.extend(library_objects);
    core_objects.extend(extra_link_inputs.iter().cloned());
    if let Some(archive) = project_as_lib_archive {
        // GCC accepts .a in the same positional slot as .o files.
        core_objects.push(archive);
    }
    let link_result = {
        let _g = perf.phase("link");
        crate::linker::Linker::link_all(
            linker,
            &sketch_objects,
            &core_objects,
            &ctx.build_dir,
            &crate::linker::LinkExtraArgs {
                flags: ctx.overlay_link_flags.clone(),
                libs: ctx.overlay_link_libs.clone(),
            },
            params.symbol_analysis,
        )?
    };

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
