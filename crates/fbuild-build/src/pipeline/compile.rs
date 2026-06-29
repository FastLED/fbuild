//! Source compilation helpers and `compile_commands.json` generation.

use std::path::{Path, PathBuf};

use fbuild_core::{BuildLog, Result};

use crate::compile_database::{self, CompileDatabase, TargetArchitecture};
use crate::compiler::Compiler;
use crate::flag_overlay::LanguageExtraFlags;

/// Compile a list of sources in parallel with incremental rebuild detection.
///
/// Thin wrapper over [`crate::parallel::compile_sources_parallel`] that flushes
/// collected warnings into the shared build log Mutex. Used by
/// [`super::run_sequential_build_with_libs`]; ESP32 calls `compile_sources_parallel`
/// directly because it interleaves multiple compile phases through the same
/// log Mutex.
pub async fn compile_sources(
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
    )
    .await?;
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
pub async fn compile_local_libraries(
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
        .await
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

/// Log the version of a GCC toolchain by running `gcc -dumpversion`.
pub async fn log_toolchain_version(gcc_path: &Path, label: &str, build_log: &mut BuildLog) {
    // FastLED/fbuild#809: `gcc -dumpversion` is a trivial probe; bound
    // it tightly so a wedged toolchain binary cannot stall build init.
    if let Ok(ver_out) = fbuild_core::subprocess::run_command(
        &[gcc_path.to_string_lossy().as_ref(), "-dumpversion"],
        None,
        None,
        Some(std::time::Duration::from_secs(5)),
    )
    .await
    {
        let version = ver_out.stdout.trim().to_string();
        if !version.is_empty() {
            crate::build_output::log_toolchain_version(build_log, label, &version);
        }
    }
}
