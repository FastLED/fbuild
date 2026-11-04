//! The sequential compile → link → result pipeline used by AVR, Teensy,
//! RP2040, STM32, ESP8266, CH32V, NRF52, SAM, Renesas, and Apollo3.

use std::path::{Path, PathBuf};
use std::time::Instant;

use fbuild_core::Result;

use crate::compile_database::TargetArchitecture;
use crate::compiler::Compiler;
use crate::source_scanner::SourceCollection;
use crate::{BuildParams, BuildResult};

use super::compile::{compile_local_libraries, compile_sources, generate_compile_db};
use super::context::BuildContext;
use super::library::{LibraryBuildEnv, compile_project_as_library_link_inputs};
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
/// [`compile_project_as_library_link_inputs`] directly.
///
/// When `lib_env` is `Some`, the project's own `src/` is compiled as a library
/// archive (matching PlatformIO's project-as-library convention) and linked
/// with the rest of the build. See [`compile_project_as_library_link_inputs`] and
/// ISSUES.md Issue 1.
#[allow(clippy::too_many_arguments)]
pub async fn run_sequential_build_with_libs(
    compiler: &(dyn Compiler + Send + Sync),
    linker: &(dyn crate::linker::Linker + Send + Sync),
    ctx: BuildContext,
    params: &BuildParams,
    sources: &SourceCollection,
    extra_link_inputs: &[PathBuf],
    lib_deps: super::library::LibDeps,
    lib_env: Option<&LibraryBuildEnv<'_>>,
    arch: TargetArchitecture,
    platform_label: &str,
    start: Instant,
) -> Result<BuildResult> {
    run_sequential_build_with_optional_framework_libs(
        compiler,
        linker,
        ctx,
        params,
        sources,
        extra_link_inputs,
        lib_deps,
        lib_env,
        None,
        arch,
        platform_label,
        start,
    )
    .await
}

/// With `Some`, archive selected local and framework library objects together
/// so unused local code cannot extract SDK startup constructors. With `None`,
/// retain the legacy direct-object linkage. The framework source slice must be
/// a subset of the core/variant sources; `Some(&[])` still archives local libs.
/// The library environment supplies the platform's LTO-aware archiver.
#[allow(clippy::too_many_arguments)]
#[expect(
    clippy::cognitive_complexity,
    clippy::too_many_lines,
    reason = "baseline, zackees/ci.yml#229"
)]
pub async fn run_sequential_build_with_optional_framework_libs(
    compiler: &(dyn Compiler + Send + Sync),
    linker: &(dyn crate::linker::Linker + Send + Sync),
    mut ctx: BuildContext,
    params: &BuildParams,
    sources: &SourceCollection,
    extra_link_inputs: &[PathBuf],
    lib_deps: super::library::LibDeps,
    lib_env: Option<&LibraryBuildEnv<'_>>,
    optional_framework_sources: Option<&[PathBuf]>,
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
    // FastLED/fbuild#574: single source of truth for the compile overlays so
    // `[env:*] build_flags` reach core/framework + libs (user_overlay) and
    // sketch + local libs (src_overlay) uniformly.
    let (user_overlay, src_overlay) = ctx.compile_overlays();

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
            &sources.ino_preludes,
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

    // Compile phases share one job gate and log (#1468).
    let jobs = crate::parallel::effective_jobs(params.jobs);
    let compile_slots = std::sync::Arc::new(tokio::sync::Semaphore::new(jobs));
    let core_cache = crate::framework_core_cache::FrameworkCoreCache::new(
        &params.project_dir,
        platform_label,
        &params.env_name,
        params.profile,
        compiler,
        &core_and_variant,
        &user_overlay,
    );
    if params.clean_all {
        let _g = perf.phase("core-cache-remove");
        match core_cache.remove() {
            Ok(()) => tracing::info!(
                "removed framework core cache key={} at {}",
                core_cache.key(),
                core_cache.path().display()
            ),
            Err(error) => tracing::warn!(
                "failed to remove framework core cache key={} at {}: {}",
                core_cache.key(),
                core_cache.path().display(),
                error
            ),
        }
    }
    if params.clean_only {
        if params.build_dir.exists() {
            std::fs::remove_dir_all(&params.build_dir)?;
        }
        return Ok(BuildResult {
            success: true,
            firmware_path: None,
            elf_path: None,
            size_info: None,
            symbol_map: None,
            build_time_secs: start.elapsed().as_secs_f64(),
            message: format!(
                "cleaned {} ({})",
                params.env_name,
                params.profile.as_dir_name()
            ),
            compile_database_path: None,
            build_log: ctx.build_log,
        });
    }
    let build_log_mutex = std::sync::Mutex::new(ctx.build_log);
    {
        let cache = &core_cache;
        let _g = perf.phase("core-cache-hydrate");
        let outcome = cache.hydrate(
            &ctx.core_build_dir,
            compiler,
            &core_and_variant,
            &user_overlay,
        );
        build_log_mutex
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(cache.hydrate_summary(&outcome));
        match &outcome {
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

    // Framework core + variant, then the core cache store (which copies
    // everything in `core_build_dir`, so it waits for both). The sketch and
    // local libraries compile alongside and fill the permits the framework
    // tail leaves idle; the store overlaps them too. Each branch times
    // itself, so the per-phase perf entries overlap in wall time.
    //
    // SAFETY-relevant: every branch must run to completion. The compile
    // helpers hand `'static`-extended borrows of the build log and flags to
    // spawned tasks and are only sound if awaited until those tasks finish
    // (see `compile_sources_parallel_shared`). `try_join!` would drop the
    // sibling branches on the first error while their tasks still run, so
    // use `join!` and propagate errors afterwards.
    let framework = async {
        let (core, variant) = tokio::join!(
            timed(compile_sources(
                compiler,
                &sources.core_sources,
                &ctx.core_build_dir,
                &user_overlay,
                &compile_slots,
                &build_log_mutex,
            )),
            timed(compile_sources(
                compiler,
                &sources.variant_sources,
                &ctx.core_build_dir,
                &user_overlay,
                &compile_slots,
                &build_log_mutex,
            )),
        );
        let (core, variant) = (core?, variant?);
        let store_started = std::time::Instant::now();
        let cache = &core_cache;
        let outcome = cache.store(&ctx.core_build_dir);
        build_log_mutex
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(cache.store_summary(&outcome));
        match &outcome {
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
        Ok::<_, fbuild_core::FbuildError>((core, variant, store_started.elapsed()))
    };

    let sketch = timed(compile_sources(
        compiler,
        &sources.sketch_sources,
        &ctx.src_build_dir,
        &src_overlay,
        &compile_slots,
        &build_log_mutex,
    ));

    // Compile local libraries normally; opted-in platforms archive them below.
    let declared_lib_deps = ctx
        .config
        .get_lib_deps(&params.env_name)
        .unwrap_or_default();
    let local_libraries = crate::framework_libs::select_local_libraries(
        &params.project_dir,
        &ctx.src_dir,
        &declared_lib_deps,
    );
    let libraries = timed(compile_local_libraries(
        compiler,
        &local_libraries,
        &ctx.build_dir,
        &src_overlay,
        &compile_slots,
        &build_log_mutex,
    ));

    let archive_override = ctx.lib_archive;
    let lib_deps_compile = timed(
        lib_deps
            .with_archive_override(archive_override)
            .compile(&compile_slots),
    );
    let existing_lib_names = local_lib_dir_names(&params.project_dir);
    let project_as_lib = timed(async {
        match lib_env {
            Some(env) => {
                compile_project_as_library_link_inputs(
                    &params.project_dir,
                    &ctx.src_dir,
                    &ctx.build_dir,
                    env,
                    &existing_lib_names,
                    &compile_slots,
                    archive_override,
                )
                .await
            }
            None => Ok(Vec::new()),
        }
    });

    let (
        ((mut core_objects, core_time), (variant_objects, variant_time), store_time),
        (sketch_objects, sketch_time),
        (library_objects, library_time),
        (lib_deps_archives, lib_deps_time),
        (project_as_lib_archive, project_as_lib_time),
    ) = {
        let (framework, sketch, libraries, lib_deps_compile, project_as_lib) = tokio::join!(
            framework,
            sketch,
            libraries,
            lib_deps_compile,
            project_as_lib
        );
        (
            framework?,
            sketch?,
            libraries?,
            lib_deps_compile?,
            project_as_lib?,
        )
    };
    perf.record("compile-core", core_time);
    perf.record("compile-variant", variant_time);
    perf.record("core-cache-store", store_time);
    perf.record("compile-sketch", sketch_time);
    perf.record("compile-local-libs", library_time);
    perf.record("compile-lib-deps", lib_deps_time);
    perf.record("project-as-lib", project_as_lib_time);
    core_objects.extend(variant_objects);

    // Unwrap the build log Mutex back into the context for the remaining
    // single-threaded phases (link, result assembly).
    ctx.build_log = build_log_mutex
        .into_inner()
        .unwrap_or_else(|e| e.into_inner());

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
            &sources.ino_preludes,
            &ctx.core_build_dir,
            &ctx.src_build_dir,
            &ctx.build_dir,
            &params.project_dir,
            arch,
        )?
    };

    // Link
    crate::build_output::log_linking(&mut ctx.build_log, "Linking firmware.elf");
    let direct_local_objects =
        direct_library_objects(&local_libraries, &ctx.build_dir, archive_override)?;
    let optional_objects: Option<Vec<PathBuf>> = optional_framework_sources.map(|sources| {
        optional_archive_objects(
            sources,
            &ctx.core_build_dir,
            &library_objects,
            &direct_local_objects,
        )
    });
    core_objects.extend(library_objects);
    core_objects.extend(extra_link_inputs.iter().cloned());
    core_objects.extend(lib_deps_archives);
    core_objects.extend(project_as_lib_archive);
    if let Some(optional_objects) = optional_objects.filter(|objects| !objects.is_empty()) {
        let env = lib_env.ok_or_else(|| {
            fbuild_core::FbuildError::BuildFailed(
                "optional framework libraries require an LTO-aware library build environment"
                    .into(),
            )
        })?;
        core_objects = archive_optional_library_objects(
            core_objects,
            &optional_objects,
            &ctx.build_dir,
            env.ar_path,
        )
        .await?;
    }
    // FastLED/fbuild#1465: `link` covers `Linker::link_all` end to end
    // (link + objcopy + size). Mandatory core objects remain direct;
    // the optional library archive has already been prepared above.
    // FastLED/fbuild#1894: `link_all` records its own `link-elf` /
    // `link-convert` / `link-size` sub-phases into the same timer.
    let link_started = Instant::now();
    let link_result = crate::linker::Linker::link_all(
        linker,
        &sketch_objects,
        &core_objects,
        &ctx.build_dir,
        &crate::linker::LinkExtraArgs {
            flags: ctx.overlay_link_flags.clone(),
            libs: ctx.overlay_link_libs.clone(),
            bloat_analysis: params.bloat_analysis,
        },
        params.symbol_analysis,
        &mut perf,
    )
    .await?;
    perf.record("link", link_started.elapsed());

    // Emit build_info_<env>.json (and the generic fallback) so downstream
    // tools (FastLED `ci/compiled_size.py`, etc.) can find the toolchain
    // and firmware paths after a successful link. Failures degrade to
    // tracing::warn! inside `emit_build_info` — never fail the build over
    // a metadata-file write. See FastLED/fbuild#297.
    {
        let _g = perf.phase("build-info");
        let prog_path = crate::build_info::pick_prog_path(
            link_result.elf_path.as_deref(),
            link_result.hex_path.as_deref(),
            link_result.bin_path.as_deref(),
        );
        if let Some(prog) = prog_path {
            let info = crate::build_info::BuildInfo::new(
                &prog,
                linker.link_driver_path(),
                linker.link_driver_path(),
                linker.ar_tool_path(),
                linker.objcopy_tool_path(),
                linker.size_tool_path(),
                compiler.c_flags(),
                compiler.cpp_flags(),
                ctx.overlay_link_flags.clone(),
                ctx.overlay_link_libs.clone(),
                platform_label.to_string(),
                ctx.board.board.clone(),
                params.env_name.clone(),
            );
            if let Err(e) =
                crate::build_info::emit_build_info(&params.project_dir, &params.env_name, &info)
            {
                tracing::warn!("emit_build_info failed: {e}");
            }
        }
    }

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

/// Preserve mandatory core startup objects, and place local and SDK objects in
/// one archive after every consumer. GNU archive extraction resolves dependencies
/// among its members without imposing the scanner's library discovery order.
fn direct_library_objects(
    libraries: &[fbuild_packages::library::FrameworkLibrary],
    build_dir: &Path,
    archive_override: Option<bool>,
) -> Result<std::collections::HashSet<PathBuf>> {
    let mut direct = std::collections::HashSet::new();
    for library in libraries {
        let info = fbuild_packages::library::library_info::InstalledLibrary::new(
            &library.dir,
            &library.name,
        );
        if !info.archive_enabled(archive_override)? {
            let output = build_dir.join("lib").join(&library.name);
            direct.extend(
                library
                    .source_files
                    .iter()
                    .map(|source| crate::compiler::CompilerBase::object_path(source, &output)),
            );
        }
    }
    Ok(direct)
}

fn optional_archive_objects(
    sources: &[PathBuf],
    core_build_dir: &Path,
    library_objects: &[PathBuf],
    direct_library_objects: &std::collections::HashSet<PathBuf>,
) -> Vec<PathBuf> {
    // The Arduino core.a contract is independent of library archiving.
    sources
        .iter()
        .map(|source| crate::compiler::CompilerBase::object_path(source, core_build_dir))
        .chain(
            library_objects
                .iter()
                .filter(|object| !direct_library_objects.contains(*object))
                .cloned(),
        )
        .collect()
}

async fn archive_optional_library_objects(
    inputs: Vec<PathBuf>,
    optional_objects: &[PathBuf],
    build_dir: &Path,
    ar_path: &Path,
) -> Result<Vec<PathBuf>> {
    let optional: std::collections::HashSet<&PathBuf> = optional_objects.iter().collect();
    let (objects, mut inputs): (Vec<_>, Vec<_>) = inputs
        .into_iter()
        .partition(|input| optional.contains(input));
    if objects.len() != optional.len() {
        return Err(fbuild_core::FbuildError::BuildFailed(
            "optional library object missing from compiled link inputs".into(),
        ));
    }
    if !objects.is_empty() {
        let archive = build_dir.join("libOptionalLibraries.a");
        crate::linker::LinkerBase::archive_if_stale(
            ar_path,
            &objects,
            &archive,
            "optional library archiver",
        )
        .await?;
        inputs.push(archive);
    }
    Ok(inputs)
}

/// Await `future`, returning its value with the wall time it took.
async fn timed<T>(
    future: impl std::future::Future<Output = Result<T>>,
) -> Result<(T, std::time::Duration)> {
    let started = std::time::Instant::now();
    let value = future.await?;
    Ok((value, started.elapsed()))
}

/// Lower-cased names of the project's `lib/*` directories; the
/// project-as-library archive yields to a `lib/` entry of the same name.
fn local_lib_dir_names(project_dir: &Path) -> std::collections::HashSet<String> {
    let mut names = std::collections::HashSet::new();
    if let Ok(entries) = std::fs::read_dir(project_dir.join("lib")) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    names.insert(name.to_lowercase());
                }
            }
        }
    }
    names
}

#[cfg(test)]
mod optional_framework_tests {
    use super::*;
    use crate::compiler::CompilerBase;
    use fbuild_core::subprocess::run_command;

    #[tokio::test]
    #[ignore = "#1885: requires native GCC and a GNU-compatible archiver"]
    async fn disabling_library_archives_preserves_core_isr_extraction() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let cc = std::env::var("FBUILD_TEST_CC").unwrap_or_else(|_| "gcc".into());
        let ar = gcc_archiver(&cc).await;
        let core = root.join("WInterrupts.c");
        let sketch = root.join("sketch.c");
        let library_dir = root.join("Direct");
        std::fs::create_dir_all(&library_dir).unwrap();
        std::fs::write(
            library_dir.join("library.json"),
            r#"{"build":{"libArchive":true}}"#,
        )
        .unwrap();
        let library = library_dir.join("library.c");
        std::fs::write(&core, "int EIC_Handler(void) { return 1; }").unwrap();
        std::fs::write(
            &sketch,
            "int EIC_Handler(void) { return 2; } __attribute__((weak)) int hook(void) { return 1; } int main(void) { return EIC_Handler() != 2 || hook() != 3; }",
        )
        .unwrap();
        std::fs::write(&library, "int hook(void) { return 3; }").unwrap();
        let mut objects = Vec::new();
        for source in [&core, &sketch, &library] {
            let output = if source == &library {
                root.join("lib/Direct")
            } else {
                root.to_path_buf()
            };
            std::fs::create_dir_all(&output).unwrap();
            let object = CompilerBase::object_path(source, &output);
            run(&[
                cc.clone(),
                "-c".into(),
                source.display().to_string(),
                "-o".into(),
                object.display().to_string(),
            ])
            .await;
            objects.push(object);
        }
        let record = fbuild_packages::library::FrameworkLibrary {
            name: "Direct".into(),
            dir: library_dir,
            source_files: vec![library],
            include_dirs: Vec::new(),
        };
        let direct = direct_library_objects(&[record], root, Some(false)).unwrap();
        let optional = optional_archive_objects(&[core], root, &[objects[2].clone()], &direct);
        assert_eq!(optional, [objects[0].clone()]);
        let inputs = archive_optional_library_objects(objects, &optional, root, &ar)
            .await
            .unwrap();
        assert!(inputs.iter().any(|input| direct.contains(input)));
        assert!(
            inputs
                .iter()
                .any(|input| input.extension().is_some_and(|extension| extension == "a"))
        );
        let executable = root.join("core-isr-probe");
        let mut args = vec![cc, "-o".into(), executable.display().to_string()];
        args.extend(inputs.iter().map(|input| input.display().to_string()));
        run(&args).await;
        run(&[executable.display().to_string()]).await;
    }

    async fn run(args: &[String]) -> String {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let result = run_command(&args, None, None, Some(std::time::Duration::from_secs(30)))
            .await
            .unwrap();
        assert!(result.success(), "{args:?}: {}", result.stderr);
        result.stdout
    }

    async fn gcc_archiver(cc: &str) -> PathBuf {
        // Compiler wrappers need not expose gcc-ar on PATH. Ask GCC for its
        // actual frontend, then use the companion archiver in that toolchain.
        let frontend = run(&[cc.into(), "-print-prog-name=cc1".into()]).await;
        let frontend = Path::new(frontend.trim());
        if frontend.is_absolute() {
            let name = fbuild_core::platform::executable::native_name("gcc-ar");
            for ancestor in frontend.ancestors().skip(1) {
                let candidate = ancestor.join("bin").join(&name);
                if candidate.is_file() {
                    return candidate;
                }
            }
        }
        let reported = run(&[cc.into(), "-print-prog-name=gcc-ar".into()]).await;
        PathBuf::from(reported.trim())
    }

    #[tokio::test]
    #[ignore = "requires native GCC and matching gcc-ar; explicitly run in Check Ubuntu CI"]
    async fn optional_framework_archive_preserves_only_referenced_initializers() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let cc = std::env::var("FBUILD_TEST_CC").unwrap_or_else(|_| "gcc".into());
        let ar = gcc_archiver(&cc).await;
        let core = root.join("core.c");
        let optional = root.join("optional.c");
        let dependency = root.join("dependency.c");
        let local_used = root.join("local_used.c");
        let local_sd = root.join("local_sd.c");
        let local_callback = root.join("local_callback.c");
        std::fs::write(
            &core,
            "int state; __attribute__((constructor)) void core_init(void) { state |= 1; }",
        )
        .unwrap();
        std::fs::write(&optional, "extern int state; extern int dependency(void); extern int local_callback(void); __attribute__((constructor)) void optional_init(void) { state |= 2; } int optional(void) { return dependency() + local_callback() - 13; }").unwrap();
        std::fs::write(&dependency, "int dependency(void) { return 7; }").unwrap();
        std::fs::write(&local_used, "int local_value(void) { return 11; }").unwrap();
        std::fs::write(
            &local_sd,
            "extern int optional(void); int local_sd(void) { return optional(); }",
        )
        .unwrap();
        std::fs::write(&local_callback, "int local_callback(void) { return 13; }").unwrap();
        let sources = [
            &core,
            &optional,
            &dependency,
            &local_used,
            &local_sd,
            &local_callback,
        ];
        let mut objects = Vec::new();
        for source in sources {
            let object = CompilerBase::object_path(source, root);
            run(&[
                cc.clone(),
                "-flto".into(),
                "-c".into(),
                source.display().to_string(),
                "-o".into(),
                object.display().to_string(),
            ])
            .await;
            objects.push(object);
        }
        let optional_objects: Vec<_> = [optional, dependency, local_used, local_sd, local_callback]
            .iter()
            .map(|source| CompilerBase::object_path(source, root))
            .collect();
        let inputs = archive_optional_library_objects(objects, &optional_objects, root, &ar)
            .await
            .unwrap();
        for (name, body) in [
            (
                "unused",
                "extern int state; extern int local_value(void); int main(void) { return state != 1 || local_value() != 11; }",
            ),
            (
                "used",
                "extern int state; extern int local_value(void); extern int local_sd(void); int main(void) { return state != 3 || local_value() != 11 || local_sd() != 7; }",
            ),
        ] {
            let main = root.join(format!("{name}.c"));
            let exe = root.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
            std::fs::write(&main, body).unwrap();
            let mut args = vec![cc.clone(), "-flto".into(), main.display().to_string()];
            args.extend(inputs.iter().map(|path| path.display().to_string()));
            args.extend(["-o".into(), exe.display().to_string()]);
            run(&args).await;
            run(&[exe.display().to_string()]).await;
        }
    }
}
