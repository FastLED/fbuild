//! Top-level library dependency orchestrator.
//!
//! Coordinates: spec parsing → download → include discovery → compile → archive.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use fbuild_core::{FbuildError, Result};
use tokio::runtime::Runtime;

use super::library_compiler;
use super::library_downloader;
use super::library_info::InstalledLibrary;
use super::library_spec::LibrarySpec;

/// Module-level fallback runtime for sync bridge entry points.
///
/// Constructed lazily on first sync invocation that occurs outside an
/// existing Tokio runtime. Reusing one runtime across calls is dramatically
/// cheaper than building/tearing one down per invocation (each construction
/// spawns worker threads + an I/O reactor). `OnceLock` makes this thread-safe
/// for free.
fn fallback_runtime() -> Result<&'static Runtime> {
    static RT: OnceLock<Runtime> = OnceLock::new();
    if let Some(rt) = RT.get() {
        return Ok(rt);
    }
    let rt = Runtime::new()
        .map_err(|e| FbuildError::PackageError(format!("failed to create tokio runtime: {}", e)))?;
    // If another thread won the race, our `rt` is dropped and we use theirs.
    Ok(RT.get_or_init(|| rt))
}

/// Resolve a local dependency relative to its project and return a canonical
/// absolute root. Compiler invocations use a separate build directory as
/// their working directory, so preserving a relative `lib_deps` path here
/// would make its include directories resolve against the wrong directory.
fn resolve_local_library_dir(project_dir: &Path, local_path: &Path, name: &str) -> Result<PathBuf> {
    let lib_dir = if local_path.is_absolute() {
        local_path.to_path_buf()
    } else {
        project_dir.join(local_path)
    };
    if !lib_dir.is_dir() {
        return Err(FbuildError::PackageError(format!(
            "local library '{}' does not exist or is not a directory: {}",
            name,
            lib_dir.display()
        )));
    }
    lib_dir
        .canonicalize()
        .map(|path| fbuild_core::path::strip_unc_prefix(&path))
        .map_err(|e| {
            FbuildError::PackageError(format!(
                "failed to canonicalize local library '{}': {} ({})",
                name,
                lib_dir.display(),
                e
            ))
        })
}

/// Result of library resolution and compilation.
pub struct LibraryResult {
    /// All include directories from all libraries (for compiler `-I` flags).
    pub include_dirs: Vec<PathBuf>,
    /// Translation units compiled into the returned archives.
    ///
    /// Platform orchestrators use these as LDF seeds when an external library
    /// includes a framework-bundled header.
    pub source_files: Vec<PathBuf>,
    /// All compiled library archives (`.a` files) for the linker.
    pub archives: Vec<PathBuf>,
}

/// Parse `lib_deps` and drop `lib_ignore` entries.
pub fn parse_lib_specs(lib_specs: &[String], lib_ignore: &[String]) -> Vec<LibrarySpec> {
    lib_specs
        .iter()
        .filter_map(|s| LibrarySpec::parse(s))
        .filter(|spec| {
            !lib_ignore
                .iter()
                .any(|ig| ig.eq_ignore_ascii_case(&spec.name))
        })
        .collect()
}

/// Libraries resolved and installed but not yet compiled.
///
/// `include_dirs` and `source_files` are known before any compile, so a build
/// can construct its compilers and run library selection first, then compile
/// the libraries in the same job pool as the rest of the build
/// (FastLED/fbuild#1559).
pub struct ResolvedLibraries {
    /// All include directories from all libraries (for compiler `-I` flags).
    pub include_dirs: Vec<PathBuf>,
    /// Translation units the plan compiles (LDF seeds).
    pub source_files: Vec<PathBuf>,
    /// The compile work, deferred until a job gate is available.
    pub plan: LibraryCompilePlan,
}

/// Deferred compilation of resolved libraries; see [`Self::compile`].
pub struct LibraryCompilePlan {
    libraries: Vec<PlannedLibrary>,
    all_include_dirs: Vec<PathBuf>,
    gcc_path: PathBuf,
    gxx_path: PathBuf,
    ar_path: PathBuf,
    c_flags: Vec<String>,
    cpp_flags: Vec<String>,
    verbose: bool,
    compiler_cache: Option<PathBuf>,
}

struct PlannedLibrary {
    name: String,
    sources: Vec<PathBuf>,
    build_dir: PathBuf,
}

impl LibraryCompilePlan {
    /// Whether there is nothing to compile.
    pub fn is_empty(&self) -> bool {
        self.libraries.is_empty()
    }

    /// Names of the libraries that [`Self::compile`] turns into an archive
    /// (`lib{name}.a`), in library order. A library without sources yields no
    /// archive and is left out, so this is known before anything compiles.
    pub fn archive_names(&self) -> Vec<String> {
        self.libraries
            .iter()
            .filter(|lib| !lib.sources.is_empty())
            .map(|lib| lib.name.clone())
            .collect()
    }

    /// Compile every library concurrently, each TU drawing a permit from
    /// `gate`. Archives come back in library order (the link order).
    ///
    /// Every library goes through the compiler's own up-to-date check, so an
    /// unchanged library costs only stats and an edited local or symlinked
    /// one is rebuilt (FastLED/fbuild#1560).
    pub async fn compile(self, gate: &library_compiler::JobGate) -> Result<Vec<PathBuf>> {
        let plan = &self;
        // join_all runs every compile to completion even if one fails.
        let archives = futures::future::join_all(plan.libraries.iter().map(|lib| async move {
            library_compiler::compile_library_gated(
                &lib.name,
                &lib.sources,
                &plan.all_include_dirs,
                &plan.gcc_path,
                &plan.gxx_path,
                &plan.ar_path,
                &plan.c_flags,
                &plan.cpp_flags,
                &lib.build_dir,
                plan.verbose,
                gate,
                plan.compiler_cache.as_deref(),
                None,
                None,
                None,
            )
            .await
        }))
        .await;
        let mut out = Vec::new();
        for archive in archives {
            out.extend(archive?);
        }
        Ok(out)
    }
}

/// Download (or resolve) every library and plan its compilation.
///
/// Flow:
/// 1. Download every library with [`download_libraries`]
/// 2. Collect all include dirs (needed before compilation for cross-includes)
/// 3. Plan one compile per library that has sources
#[allow(clippy::too_many_arguments)]
pub async fn resolve_libraries(
    lib_specs: &[String],
    lib_ignore: &[String],
    gcc_path: &Path,
    gxx_path: &Path,
    ar_path: &Path,
    c_flags: &[String],
    cpp_flags: &[String],
    base_includes: &[PathBuf],
    project_dir: &Path,
    libs_dir: &Path,
    verbose: bool,
    compiler_cache: Option<&Path>,
) -> Result<ResolvedLibraries> {
    let installed = download_libraries(lib_specs, lib_ignore, project_dir, libs_dir).await?;

    let mut all_include_dirs: Vec<PathBuf> = base_includes.to_vec();
    for lib in &installed {
        all_include_dirs.extend(lib.get_include_dirs());
    }

    let libraries = installed
        .iter()
        .filter_map(|lib| {
            if lib.is_header_only() {
                tracing::info!("library {} is header-only", lib.name);
                return None;
            }
            Some(PlannedLibrary {
                name: lib.name.clone(),
                sources: lib.get_source_files(),
                build_dir: lib.build_dir.clone(),
            })
        })
        .collect();

    Ok(ResolvedLibraries {
        // Library includes only, not base includes.
        include_dirs: installed
            .iter()
            .flat_map(|lib| lib.get_include_dirs())
            .collect(),
        source_files: installed
            .iter()
            .flat_map(|lib| lib.get_source_files())
            .collect(),
        plan: LibraryCompilePlan {
            libraries,
            all_include_dirs,
            gcc_path: gcc_path.to_path_buf(),
            gxx_path: gxx_path.to_path_buf(),
            ar_path: ar_path.to_path_buf(),
            c_flags: c_flags.to_vec(),
            cpp_flags: cpp_flags.to_vec(),
            verbose,
            compiler_cache: compiler_cache.map(Path::to_path_buf),
        },
    })
}

/// Ensure all library dependencies are downloaded and compiled: [`resolve_libraries`]
/// then [`LibraryCompilePlan::compile`] on a gate of `jobs` permits.
#[allow(clippy::too_many_arguments)]
pub async fn ensure_libraries(
    lib_specs: &[String],
    lib_ignore: &[String],
    gcc_path: &Path,
    gxx_path: &Path,
    ar_path: &Path,
    c_flags: &[String],
    cpp_flags: &[String],
    base_includes: &[PathBuf],
    project_dir: &Path,
    libs_dir: &Path,
    verbose: bool,
    jobs: usize,
    compiler_cache: Option<&Path>,
) -> Result<LibraryResult> {
    let resolved = resolve_libraries(
        lib_specs,
        lib_ignore,
        gcc_path,
        gxx_path,
        ar_path,
        c_flags,
        cpp_flags,
        base_includes,
        project_dir,
        libs_dir,
        verbose,
        compiler_cache,
    )
    .await?;
    let archives = resolved
        .plan
        .compile(&library_compiler::job_gate(jobs))
        .await?;
    Ok(LibraryResult {
        include_dirs: resolved.include_dirs,
        source_files: resolved.source_files,
        archives,
    })
}

/// Download (or resolve locally) every `lib_deps` library and its transitive
/// dependencies, without compiling. `fbuild install` stops here; builds go on
/// to compile in [`ensure_libraries`] (FastLED/fbuild#1433).
pub async fn download_libraries(
    lib_specs: &[String],
    lib_ignore: &[String],
    project_dir: &Path,
    libs_dir: &Path,
) -> Result<Vec<InstalledLibrary>> {
    // 1. Parse specs, filter ignored
    let specs = parse_lib_specs(lib_specs, lib_ignore);
    if specs.is_empty() {
        return Ok(Vec::new());
    }

    tracing::info!("resolving {} library dependencies", specs.len());

    // 2. Resolve named local libraries and download remote libraries in parallel.
    // Local libraries compile into `libs_dir`, never their checked-out source
    // directory, so a build cannot leave generated artifacts in a dependency.
    std::fs::create_dir_all(libs_dir)?;
    let mut installed: Vec<InstalledLibrary> = Vec::new();
    let mut downloaded_names: std::collections::HashSet<String> = std::collections::HashSet::new();

    let libs_dir_owned = libs_dir.to_path_buf();
    let mut tasks: tokio::task::JoinSet<
        std::result::Result<(std::path::PathBuf, String, String), fbuild_core::FbuildError>,
    > = tokio::task::JoinSet::new();
    for spec in &specs {
        if let Some(local_path) = &spec.local_path {
            let lib_dir = resolve_local_library_dir(project_dir, local_path, &spec.name)?;
            let sanitized = spec.sanitized_name();
            installed.push(InstalledLibrary::with_build_dir(
                &lib_dir,
                &sanitized,
                &libs_dir.join(&sanitized),
            ));
            downloaded_names.insert(spec.name.to_lowercase());
            continue;
        }
        let spec_clone = spec.clone();
        let dir = libs_dir_owned.clone();
        tasks.spawn(async move {
            let lib_dir = library_downloader::download_library(&spec_clone, &dir).await?;
            Ok((
                lib_dir,
                spec_clone.sanitized_name(),
                spec_clone.name.to_lowercase(),
            ))
        });
    }

    while let Some(joined) = tasks.join_next().await {
        let (lib_dir, sanitized, name_lower) = joined.map_err(|e| {
            fbuild_core::FbuildError::PackageError(format!("library download task failed: {}", e))
        })??;
        installed.push(InstalledLibrary::new(&lib_dir, &sanitized));
        downloaded_names.insert(name_lower);
    }

    // 2b. Resolve transitive dependencies from library.json files
    let ignore_set: std::collections::HashSet<String> =
        lib_ignore.iter().map(|s| s.to_lowercase()).collect();
    resolve_transitive_deps(&mut installed, &mut downloaded_names, &ignore_set, libs_dir).await?;

    Ok(installed)
}

/// Resolve transitive dependencies by scanning library.json files.
///
/// For each installed library, reads its `library.json` (checking both
/// `lib_dir/library.json` and `lib_dir/src/library.json`) for a `dependencies`
/// array. Downloads any new dependencies and adds them to the installed list.
/// Processes recursively until no new dependencies are found.
async fn resolve_transitive_deps(
    installed: &mut Vec<InstalledLibrary>,
    downloaded_names: &mut std::collections::HashSet<String>,
    lib_ignore: &std::collections::HashSet<String>,
    libs_dir: &Path,
) -> Result<()> {
    let mut queue: Vec<PathBuf> = installed.iter().map(|lib| lib.lib_dir.clone()).collect();

    while let Some(lib_dir) = queue.pop() {
        // Check both possible locations for library.json
        let candidates = [
            lib_dir.join("library.json"),
            lib_dir.join("src").join("library.json"),
        ];

        let mut deps: Vec<serde_json::Value> = Vec::new();
        for candidate in &candidates {
            if !candidate.exists() {
                continue;
            }
            let content = match std::fs::read_to_string(candidate) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let data: serde_json::Value = match serde_json::from_str(&content) {
                Ok(d) => d,
                Err(_) => continue,
            };
            if let Some(dep_list) = data.get("dependencies") {
                match dep_list {
                    serde_json::Value::Array(arr) => {
                        deps = arr.clone();
                        break;
                    }
                    serde_json::Value::Object(_) => {
                        deps = vec![dep_list.clone()];
                        break;
                    }
                    _ => {}
                }
            }
        }

        for dep in deps {
            let dep_obj = match dep.as_object() {
                Some(o) => o,
                None => continue,
            };

            // Filter by platform — only download ESP32-compatible deps
            if let Some(platforms) = dep_obj.get("platforms") {
                let dominated = match platforms {
                    serde_json::Value::Array(arr) => {
                        arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>()
                    }
                    serde_json::Value::String(s) => vec![s.as_str()],
                    _ => vec![],
                };
                if !dominated.is_empty()
                    && !dominated.iter().any(|p| *p == "espressif32" || *p == "*")
                {
                    continue;
                }
            }

            let dep_name = match dep_obj.get("name").and_then(|n| n.as_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };

            if downloaded_names.contains(&dep_name.to_lowercase()) {
                continue;
            }
            if lib_ignore.contains(&dep_name.to_lowercase()) {
                tracing::debug!("skipping ignored transitive dependency: {}", dep_name);
                continue;
            }

            let dep_owner = dep_obj.get("owner").and_then(|o| o.as_str()).unwrap_or("");
            let dep_version = dep_obj.get("version").and_then(|v| v.as_str());

            let mut spec_str = if dep_owner.is_empty() {
                dep_name.clone()
            } else {
                format!("{}/{}", dep_owner, dep_name)
            };
            if let Some(ver) = dep_version {
                spec_str = format!("{} @ {}", spec_str, ver);
            }

            tracing::info!("resolving transitive dependency: {}", spec_str);

            if let Some(spec) = LibrarySpec::parse(&spec_str) {
                match library_downloader::download_library(&spec, libs_dir).await {
                    Ok(dep_dir) => {
                        let lib = InstalledLibrary::new(&dep_dir, &spec.sanitized_name());
                        queue.push(dep_dir);
                        installed.push(lib);
                        downloaded_names.insert(dep_name.to_lowercase());
                    }
                    Err(e) => {
                        tracing::warn!(
                            "could not resolve transitive dependency '{}': {}",
                            spec_str,
                            e
                        );
                    }
                }
            }
        }
    }

    Ok(())
}

/// Synchronous wrapper for ensure_libraries (legacy sync call-sites).
///
/// New code should call the async `ensure_libraries` directly. This bridge
/// stays during the #813 migration so fbuild-build orchestrators that haven't
/// been converted yet can still link.
#[allow(clippy::too_many_arguments)]
pub fn ensure_libraries_sync(
    lib_specs: &[String],
    lib_ignore: &[String],
    gcc_path: &Path,
    gxx_path: &Path,
    ar_path: &Path,
    c_flags: &[String],
    cpp_flags: &[String],
    base_includes: &[PathBuf],
    project_dir: &Path,
    libs_dir: &Path,
    verbose: bool,
    jobs: usize,
    compiler_cache: Option<&Path>,
) -> Result<LibraryResult> {
    let fut = ensure_libraries(
        lib_specs,
        lib_ignore,
        gcc_path,
        gxx_path,
        ar_path,
        c_flags,
        cpp_flags,
        base_includes,
        project_dir,
        libs_dir,
        verbose,
        jobs,
        compiler_cache,
    );
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        tokio::task::block_in_place(|| handle.block_on(fut))
    } else {
        fallback_runtime()?.block_on(fut)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_empty_specs() {
        let result = ensure_libraries_sync(
            &[],
            &[],
            Path::new("/gcc"),
            Path::new("/g++"),
            Path::new("/ar"),
            &[],
            &[],
            &[],
            Path::new("/project"),
            Path::new("/libs"),
            false,
            1,
            None,
        )
        .unwrap();
        assert!(result.include_dirs.is_empty());
        assert!(result.archives.is_empty());
    }

    #[test]
    fn test_all_ignored() {
        let result = ensure_libraries_sync(
            &["FastLED".to_string()],
            &["FastLED".to_string()],
            Path::new("/gcc"),
            Path::new("/g++"),
            Path::new("/ar"),
            &[],
            &[],
            &[],
            Path::new("/project"),
            Path::new("/libs"),
            false,
            1,
            None,
        )
        .unwrap();
        assert!(result.include_dirs.is_empty());
        assert!(result.archives.is_empty());
    }

    #[test]
    fn test_named_relative_local_symlink_adds_include_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project = tmp.path().join("project");
        let local = project.join("local");
        let local_src = local.join("src");
        std::fs::create_dir_all(&local_src).unwrap();
        std::fs::write(local_src.join("Local.h"), "").unwrap();
        let libs_dir = tmp.path().join("build").join("libs");
        let result = ensure_libraries_sync(
            &["Local=symlink://local".to_string()],
            &[],
            Path::new("/gcc"),
            Path::new("/g++"),
            Path::new("/ar"),
            &[],
            &[],
            &[],
            &project,
            &libs_dir,
            false,
            1,
            None,
        )
        .unwrap();
        // Production canonicalizes the local library root
        // (`resolve_local_library_dir`), so canonicalize the expectation the
        // same way. Comparing the raw tempfile path fails on Windows (8.3
        // short names like `RUNNER~1` in %TEMP%) and macOS (`/var` is a
        // symlink to `/private/var`).
        let expected = fbuild_core::path::strip_unc_prefix(&local_src.canonicalize().unwrap());
        assert_eq!(result.include_dirs, vec![expected]);
    }

    // ---- FastLED/fbuild#1559 / #1560 plan-level scheduling tests ----

    /// Install a fake compiler shell script at `path` that parses `-o <obj>`,
    /// touches the object file, and appends the compiled source path to
    /// `log`. Installed via staging + rename to avoid ETXTBSY under parallel
    /// tests.
    fn install_fake_compiler(path: &Path, log: &Path) {
        let script = format!(
            "#!/bin/sh\n\
             obj=\"\"\n\
             src=\"\"\n\
             prev=\"\"\n\
             for arg in \"$@\"; do\n\
             \x20 if [ \"$prev\" = \"-o\" ]; then obj=\"$arg\"; fi\n\
             \x20 if [ \"$prev\" = \"-c\" ]; then src=\"$arg\"; fi\n\
             \x20 prev=\"$arg\"\n\
             done\n\
             echo \"$src\" >> \"{log}\"\n\
             mkdir -p \"$(dirname \"$obj\")\"\n\
             touch \"$obj\"\n",
            log = log.display()
        );
        let staging = path.with_extension("staging");
        std::fs::write(&staging, script).unwrap();
        fbuild_core::platform::fs::set_executable(&staging).unwrap();
        std::fs::rename(&staging, path).unwrap();
    }

    /// Install a fake `ar` shell script that creates its 2nd arg (the
    /// archive) as an empty file.
    fn install_fake_ar(path: &Path) {
        let script = "#!/bin/sh\ntouch \"$2\"\n";
        let staging = path.with_extension("staging");
        std::fs::write(&staging, script).unwrap();
        fbuild_core::platform::fs::set_executable(&staging).unwrap();
        std::fs::rename(&staging, path).unwrap();
    }

    fn make_local_library(project: &Path, rel: &str, name: &str) -> PathBuf {
        let lib_dir = project.join(rel);
        let src = lib_dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join(format!("{name}.cpp")), "int f() { return 1; }\n").unwrap();
        std::fs::write(src.join(format!("{name}.h")), "int f();\n").unwrap();
        lib_dir
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_plan_compiles_concurrently_and_recompiles_edited_local_library() {
        if fbuild_core::platform::host::is_windows() {
            return;
        }
        let tmp = tempfile::TempDir::new().unwrap();
        let project = tmp.path().join("project");
        make_local_library(&project, "liba", "liba");
        make_local_library(&project, "libb", "libb");
        let libs_dir = tmp.path().join("build").join("libs");
        std::fs::create_dir_all(&libs_dir).unwrap();

        let tools_dir = tmp.path().join("tools");
        std::fs::create_dir_all(&tools_dir).unwrap();
        let log = tmp.path().join("compile.log");
        let gcc = tools_dir.join("gcc");
        let gxx = tools_dir.join("g++");
        let ar = tools_dir.join("ar");
        install_fake_compiler(&gcc, &log);
        install_fake_compiler(&gxx, &log);
        install_fake_ar(&ar);

        let specs = vec![
            "LibA=symlink://liba".to_string(),
            "LibB=symlink://libb".to_string(),
        ];

        let resolved = resolve_libraries(
            &specs,
            &[],
            &gcc,
            &gxx,
            &ar,
            &[],
            &[],
            &[],
            &project,
            &libs_dir,
            false,
            None,
        )
        .await
        .unwrap();

        let archives = resolved
            .plan
            .compile(&library_compiler::job_gate(4))
            .await
            .unwrap();

        let expected_a = libs_dir.join("liba").join("libliba.a");
        let expected_b = libs_dir.join("libb").join("liblibb.a");
        assert_eq!(archives, vec![expected_a.clone(), expected_b.clone()]);
        assert!(expected_a.exists());
        assert!(expected_b.exists());

        let log_after_first = std::fs::read_to_string(&log).unwrap();
        assert!(log_after_first.contains("liba.cpp"));
        assert!(log_after_first.contains("libb.cpp"));

        // Modify LibA's source and push its mtime into the future so the
        // up-to-date check (source mtime vs. object mtime) reliably sees it
        // as stale on any filesystem timestamp granularity.
        let liba_src = project.join("liba").join("src").join("liba.cpp");
        std::fs::write(&liba_src, "int f() { return 2; }\n").unwrap();
        let future = std::time::SystemTime::now() + Duration::from_secs(3600);
        std::fs::File::options()
            .write(true)
            .open(&liba_src)
            .unwrap()
            .set_modified(future)
            .unwrap();

        let resolved2 = resolve_libraries(
            &specs,
            &[],
            &gcc,
            &gxx,
            &ar,
            &[],
            &[],
            &[],
            &project,
            &libs_dir,
            false,
            None,
        )
        .await
        .unwrap();
        let archives2 = resolved2
            .plan
            .compile(&library_compiler::job_gate(4))
            .await
            .unwrap();
        assert_eq!(archives2, vec![expected_a, expected_b]);

        let log_after_second = std::fs::read_to_string(&log).unwrap();
        let liba_compiles = log_after_second.matches("liba.cpp").count();
        let libb_compiles = log_after_second.matches("libb.cpp").count();
        assert_eq!(
            liba_compiles, 2,
            "edited LibA source must be recompiled on the second resolve+compile"
        );
        assert_eq!(
            libb_compiles, 1,
            "untouched LibB must not be recompiled on the second resolve+compile"
        );
    }

    /// FastLED/fbuild#1559: the ESP32 orchestrator skips a framework library
    /// that a `lib_deps` archive already provides, before anything compiles.
    /// Only libraries with sources yield an archive; names keep their case.
    #[test]
    fn test_archive_names_skip_libraries_without_sources() {
        let planned = |name: &str, sources: Vec<PathBuf>| PlannedLibrary {
            name: name.to_string(),
            sources,
            build_dir: PathBuf::from("build").join(name),
        };
        let plan = LibraryCompilePlan {
            libraries: vec![
                planned("FastLED", vec![PathBuf::from("a.cpp")]),
                planned("Empty", Vec::new()),
                planned("zlib", vec![PathBuf::from("z.c")]),
            ],
            all_include_dirs: Vec::new(),
            gcc_path: PathBuf::from("gcc"),
            gxx_path: PathBuf::from("g++"),
            ar_path: PathBuf::from("ar"),
            c_flags: Vec::new(),
            cpp_flags: Vec::new(),
            verbose: false,
            compiler_cache: None,
        };
        assert_eq!(plan.archive_names(), vec!["FastLED", "zlib"]);
    }
}
