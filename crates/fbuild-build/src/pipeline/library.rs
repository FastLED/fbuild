//! Library compilation helpers: `LibraryBuildEnv`, archiver selection,
//! extra library roots, and the "project-as-library" compile path.

use std::path::{Path, PathBuf};

use fbuild_core::Result;

use super::project_discovery::is_project_a_library;

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

fn resolve_extra_library_path(project_dir: &Path, entry: &str) -> PathBuf {
    let path = PathBuf::from(entry);
    let path = if path.is_absolute() {
        path
    } else {
        project_dir.join(path)
    };
    crate::compiler::absolute_from_cwd(&path)
}

fn looks_like_library_root(path: &Path) -> bool {
    path.join("library.json").is_file()
        || path.join("library.properties").is_file()
        || path.join("src").is_dir()
}

fn library_name(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("library")
        .to_lowercase()
}

fn strip_windows_extended_prefix(path: PathBuf) -> PathBuf {
    if !cfg!(windows) {
        return path;
    }
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\").or_else(|| s.strip_prefix("//?/")) {
        return PathBuf::from(rest);
    }
    path
}

/// Resolve `lib_extra_dirs`/`PLATFORMIO_LIB_EXTRA_DIRS` entries to library roots.
///
/// `pio ci --lib <path>` commonly points directly at a library root (FastLED
/// uses `--lib .`), while `lib_extra_dirs` can point at a directory containing
/// multiple libraries. Support both forms.
pub fn discover_extra_library_roots(project_dir: &Path, entries: &[String]) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for entry in entries {
        let path = resolve_extra_library_path(project_dir, entry);
        if !path.is_dir() {
            tracing::warn!(
                "lib_extra_dirs entry is not a directory: {}",
                path.display()
            );
            continue;
        }

        let mut candidates = Vec::new();
        if looks_like_library_root(&path) {
            candidates.push(path);
        } else if let Ok(children) = std::fs::read_dir(&path) {
            for child in children.flatten() {
                let child_path = child.path();
                if child_path.is_dir() && looks_like_library_root(&child_path) {
                    candidates.push(child_path);
                }
            }
        }

        for candidate in candidates {
            let key = std::fs::canonicalize(&candidate)
                .map(strip_windows_extended_prefix)
                .unwrap_or(candidate.clone());
            if seen.insert(key.clone()) {
                roots.push(key);
            }
        }
    }

    roots
}

/// Add include dirs for extra library roots to an orchestrator include list.
pub fn add_extra_library_include_dirs(library_roots: &[PathBuf], include_dirs: &mut Vec<PathBuf>) {
    for root in library_roots {
        let lib_name = library_name(root);
        let lib = fbuild_packages::library::library_info::InstalledLibrary::new(root, &lib_name);
        let mut dirs = lib.get_include_dirs();
        if !include_dirs.contains(root) {
            dirs.push(root.clone());
        }
        for dir in dirs {
            if !include_dirs.contains(&dir) {
                include_dirs.push(dir);
            }
        }
    }
}

/// Compile extra library roots from `lib_extra_dirs` into archives.
pub async fn compile_extra_libraries(
    library_roots: &[PathBuf],
    build_dir: &Path,
    env: &LibraryBuildEnv<'_>,
) -> Result<Vec<PathBuf>> {
    let mut archives = Vec::new();
    for root in library_roots {
        let lib_name = library_name(root);
        let lib_info =
            fbuild_packages::library::library_info::InstalledLibrary::new(root, &lib_name);
        let sources = lib_info.get_source_files();
        if sources.is_empty() {
            tracing::info!("extra library '{}' is header-only", lib_name);
            continue;
        }

        tracing::info!(
            "compiling extra library '{}': {} source files from {}",
            lib_name,
            sources.len(),
            root.display()
        );

        let output_dir = build_dir.join("extra_lib").join(&lib_name);
        std::fs::create_dir_all(&output_dir)?;
        match fbuild_packages::library::library_compiler::compile_library_with_jobs(
            &lib_name,
            &sources,
            env.include_dirs,
            env.gcc_path,
            env.gxx_path,
            env.ar_path,
            env.c_flags,
            env.cpp_flags,
            &output_dir,
            env.verbose,
            env.jobs,
            env.compiler_cache,
        )
        .await
        {
            Ok(Some(archive)) => archives.push(archive),
            Ok(None) => {}
            Err(e) => {
                return Err(fbuild_core::FbuildError::BuildFailed(format!(
                    "extra library '{}' compilation failed: {}",
                    lib_name, e
                )));
            }
        }
    }
    Ok(archives)
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
pub async fn compile_project_as_library(
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
    )
    .await
    {
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
mod extra_library_tests {
    use super::*;

    #[test]
    fn discovers_direct_library_root() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("library.properties"), "name=FastLED\n").unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();

        let roots = discover_extra_library_roots(tmp.path(), &[".".to_string()]);

        assert_eq!(roots.len(), 1);
        assert_eq!(
            roots[0],
            strip_windows_extended_prefix(std::fs::canonicalize(tmp.path()).unwrap())
        );
    }

    #[test]
    fn discovers_libraries_inside_storage_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = tmp.path().join("libs");
        let lib_a = storage.join("LibA");
        let lib_b = storage.join("LibB");
        std::fs::create_dir_all(lib_a.join("src")).unwrap();
        std::fs::create_dir_all(lib_b.join("src")).unwrap();

        let roots = discover_extra_library_roots(tmp.path(), &["libs".to_string()]);

        assert_eq!(roots.len(), 2);
        assert!(roots.iter().any(|root| root.ends_with("LibA")));
        assert!(roots.iter().any(|root| root.ends_with("LibB")));
    }

    #[test]
    fn adds_src_and_root_include_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let lib = tmp.path().join("FastLED");
        std::fs::create_dir_all(lib.join("src")).unwrap();
        std::fs::write(lib.join("src").join("FastLED.h"), "").unwrap();

        let mut include_dirs = Vec::new();
        add_extra_library_include_dirs(std::slice::from_ref(&lib), &mut include_dirs);

        assert!(include_dirs.contains(&lib.join("src")));
        assert!(include_dirs.contains(&lib));
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

    #[tokio::test]
    async fn test_returns_none_when_not_a_library() {
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
        )
        .await;
        assert!(matches!(result, Ok(None)));
    }

    #[tokio::test]
    async fn test_returns_none_when_src_dir_equals_project_src() {
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
        )
        .await;
        assert!(matches!(result, Ok(None)));
    }

    #[tokio::test]
    async fn test_returns_none_when_src_dir_equals_project_dir() {
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
        )
        .await;
        assert!(matches!(result, Ok(None)));
    }

    #[tokio::test]
    async fn test_returns_none_when_no_src_dir() {
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
        )
        .await;
        assert!(matches!(result, Ok(None)));
    }

    #[tokio::test]
    async fn test_returns_none_when_header_only() {
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
        )
        .await;
        assert!(matches!(result, Ok(None)));
    }

    #[tokio::test]
    async fn test_returns_none_on_collision_with_lib_dir() {
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
        )
        .await;
        assert!(matches!(result, Ok(None)));
    }

    #[tokio::test]
    async fn test_attempts_compile_when_building_example() {
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
        )
        .await;
        // Must NOT be Ok(None) — that would mean a guard skipped compile.
        // Either Err (bogus tool failed) or Ok(Some(_)) (impossible without
        // a real toolchain) is acceptable.
        if let Ok(None) = result {
            panic!("expected compile to be attempted, but a guard returned Ok(None)");
        }
    }
}
