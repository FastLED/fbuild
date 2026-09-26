//! Source compilation helpers and `compile_commands.json` generation.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Semaphore;

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
    jobs: &Arc<Semaphore>,
    build_log: &std::sync::Mutex<BuildLog>,
) -> Result<Vec<PathBuf>> {
    let result = crate::parallel::compile_sources_parallel_shared(
        compiler,
        sources,
        build_dir,
        extra_flags,
        jobs,
        Some(build_log),
    )
    .await?;
    if !result.warnings.is_empty() {
        let mut log = build_log.lock().unwrap_or_else(|e| e.into_inner());
        for w in &result.warnings {
            crate::build_output::collect_warnings(w, &mut log);
        }
    }
    Ok(result.objects)
}

/// Compile the project `lib/` libraries the build uses.
///
/// `libraries` comes from [`crate::framework_libs::select_local_libraries`],
/// so a library nothing includes is never compiled (FastLED/fbuild#1410).
/// Each library's source files are compiled in parallel via
/// [`crate::parallel::compile_sources_parallel_shared`], drawing on the
/// build's shared `jobs` semaphore (FastLED/fbuild#1468). Libraries
/// themselves are processed one after another.
pub async fn compile_local_libraries(
    compiler: &dyn Compiler,
    libraries: &[fbuild_packages::library::FrameworkLibrary],
    build_dir: &Path,
    extra_flags: &LanguageExtraFlags,
    jobs: &Arc<Semaphore>,
    build_log: &std::sync::Mutex<BuildLog>,
) -> Result<Vec<PathBuf>> {
    let mut library_objects = Vec::new();
    for library in libraries {
        let lib_name = &library.name;
        let lib_sources = &library.source_files;
        if lib_sources.is_empty() {
            continue;
        }

        let lib_build_dir = build_dir.join("lib").join(lib_name);
        std::fs::create_dir_all(&lib_build_dir)?;
        tracing::info!(
            "compiling local library '{}': {} source files",
            lib_name,
            lib_sources.len()
        );

        let result = crate::parallel::compile_sources_parallel_shared(
            compiler,
            lib_sources,
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
            let mut log = build_log.lock().unwrap_or_else(|e| e.into_inner());
            for w in &result.warnings {
                crate::build_output::collect_warnings(w, &mut log);
            }
        }
    }
    Ok(library_objects)
}

/// Generate `compile_commands.json` from core/variant and sketch sources.
///
/// Also writes the untranslated toolchain commands to
/// `compile_commands.raw.json` in `build_dir` (FastLED/fbuild#1467).
///
/// IDE-flavored: when `ino_preludes` is non-empty (i.e. the sketch had
/// `.ino` tabs preprocessed into a generated `<stem>.ino.cpp`), the
/// generated file's entry is swapped for one raw-`.ino` entry per tab so
/// clangd gives IntelliSense on the file the user actually edits
/// (FastLED/fbuild#1076 Phase 0). See
/// [`compile_database::CompileDatabase::swap_ino_entries_for_raw`].
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
    ino_preludes: &[(PathBuf, PathBuf)],
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
    if !compile_db.has_entries() {
        return Ok(None);
    }
    // FastLED/fbuild#1467: record the real toolchain invocations before the
    // clangd rewrite drops GCC-only flags such as `-flto`.
    compile_db.write_raw(build_dir)?;
    let compile_db = compile_db.translate_for_clang(arch);
    let compile_db = compile_db.swap_ino_entries_for_raw(ino_preludes);
    Ok(Some(compile_db.write_and_copy(build_dir, project_dir)?))
}

/// Log the version of a GCC toolchain (`gcc -dumpversion`).
///
/// Reuses the per-compiler memoized probe behind rebuild signatures, so a
/// warm build spawns nothing and a cold one probes each compiler once
/// (FastLED/fbuild#1466).
pub async fn log_toolchain_version(gcc_path: &Path, label: &str, build_log: &mut BuildLog) {
    let version = crate::rebuild_signature::cached_compiler_version(gcc_path).await;
    if !version.is_empty() {
        crate::build_output::log_toolchain_version(build_log, label, &version);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(db: &serde_json::Value, file_suffix: &str) -> Vec<String> {
        let entry = db
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["file"].as_str().unwrap().ends_with(file_suffix))
            .unwrap_or_else(|| panic!("no entry for {file_suffix}"));
        entry["arguments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|arg| arg.as_str().unwrap().to_string())
            .collect()
    }

    /// FastLED/fbuild#1467: `compile_commands.json` is clangd-flavored
    /// (`clang++ --target=avr`, no `-flto`). The real toolchain invocations
    /// must also be recorded, in `compile_commands.raw.json`, so flags can
    /// be compared and commands replayed.
    #[test]
    fn raw_compile_db_keeps_the_real_compiler_and_flags() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        let build = project.join(".fbuild/build/uno/release");
        let core_src = project.join("core/wiring.c");
        let sketch_src = project.join("src/main.cpp");
        std::fs::create_dir_all(core_src.parent().unwrap()).unwrap();
        std::fs::create_dir_all(sketch_src.parent().unwrap()).unwrap();
        std::fs::write(&core_src, "int x;\n").unwrap();
        std::fs::write(&sketch_src, "int y;\n").unwrap();
        let gcc = tmp.path().join("toolchain/bin/avr-gcc");
        let gxx = tmp.path().join("toolchain/bin/avr-g++");
        let c_flags = vec!["-Os".to_string(), "-flto".to_string()];
        let cpp_flags = vec![
            "-Os".to_string(),
            "-flto".to_string(),
            "-fno-exceptions".to_string(),
        ];

        generate_compile_db(
            &gcc,
            &gxx,
            &c_flags,
            &cpp_flags,
            &[],
            &LanguageExtraFlags::default(),
            &LanguageExtraFlags::default(),
            &[core_src],
            &[sketch_src],
            &[],
            &build.join("core"),
            &build.join("src"),
            &build,
            &project,
            TargetArchitecture::Avr,
        )
        .unwrap()
        .expect("database written");

        let read = |path: PathBuf| -> serde_json::Value {
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
        };
        let raw = read(build.join(CompileDatabase::RAW_FILE_NAME));
        let raw_c = arguments(&raw, "wiring.c");
        let raw_cpp = arguments(&raw, "main.cpp");
        assert_eq!(raw_c[0], gcc.to_string_lossy());
        assert_eq!(raw_cpp[0], gxx.to_string_lossy());
        assert!(raw_c.contains(&"-flto".to_string()), "{raw_c:?}");
        assert!(raw_cpp.contains(&"-flto".to_string()), "{raw_cpp:?}");

        // The clangd-oriented database is unchanged: translated, no LTO.
        let clangd = read(build.join("compile_commands.json"));
        let clangd_cpp = arguments(&clangd, "main.cpp");
        assert!(!clangd_cpp.contains(&"-flto".to_string()), "{clangd_cpp:?}");
        // The raw database stays in the build dir; the project root keeps
        // only the IDE database.
        assert!(!project.join(CompileDatabase::RAW_FILE_NAME).exists());
    }
}
