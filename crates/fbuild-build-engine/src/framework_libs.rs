//! Framework-library resolution shared across platform orchestrators.
//!
//! PlatformIO ships Arduino-style frameworks (Teensyduino, STM32duino, ...)
//! with a `libraries/` directory containing bundled libraries like `SPI` and
//! `Wire`. A sketch that does `#include <SPI.h>` must get the library's
//! include dirs on the compiler's search path and its sources linked in.
//!
//! Implementation delegates to `fbuild-library-select`, which runs a
//! PlatformIO-LDF-style two-pass walk backed by `fbuild-header-scan`. That
//! crate does path-prefix attribution (not basename matching), so libraries
//! with colliding header names no longer trample each other, and unreferenced
//! framework libraries (FNET/Snooze/RadioHead/mbedtls on teensyLC, for
//! example) stay out of the compile set. See FastLED/fbuild#205.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use fbuild_library_select::cache::{CacheKeyInputs, FileKvStore, resolve_cached};
use fbuild_library_select::resolve as resolve_library_selection;
use fbuild_packages::library::FrameworkLibrary;
use walkdir::{DirEntry, WalkDir};

/// Resolve framework library source files needed by a project.
pub fn resolve_framework_library_sources(
    libraries: &[FrameworkLibrary],
    project_dir: &Path,
    src_dir: &Path,
) -> Vec<PathBuf> {
    let roots = framework_include_scan_roots(project_dir, src_dir);
    let filtered = filter_framework_libs_shadowed_by_project(libraries, &roots);
    resolve_framework_library_sources_from_libraries(&filtered, &roots)
}

/// Resolve framework libraries using active preprocessor branches only.
pub fn resolve_framework_library_sources_active(
    libraries: &[FrameworkLibrary],
    project_dir: &Path,
    src_dir: &Path,
    defines: &HashMap<String, String>,
) -> Vec<PathBuf> {
    resolve_framework_library_sources_active_declared(libraries, project_dir, src_dir, defines, &[])
}

/// [`resolve_framework_library_sources_active`] honoring `lib_deps`.
///
/// `declared` are the `platformio.ini` `lib_deps` entries for the env being
/// built. A framework library named there is selected even though the header
/// scan never reaches it — the escape hatch for a dependency the finder
/// cannot infer, which previously had no lever at all on the Teensy/STM32
/// path (FastLED/fbuild#1214).
///
/// Seeds are every translation unit the build compiles — project sources and
/// local-library sources alike (FastLED/fbuild#1337). Headers are still never
/// seeds, which is what preserves #1094's "an inactive local library header
/// must not select a framework library".
pub fn resolve_framework_library_sources_active_declared(
    libraries: &[FrameworkLibrary],
    project_dir: &Path,
    src_dir: &Path,
    defines: &HashMap<String, String>,
    declared: &[String],
) -> Vec<PathBuf> {
    resolve_framework_library_selection_active_declared(
        libraries,
        project_dir,
        src_dir,
        defines,
        declared,
    )
    .source_files
}

/// Resolve the selected framework-library records using active branches and
/// explicit declarations.
///
/// Most orchestrators only need the flattened source list. ESP32 also needs
/// the selected include roots and library names so it can retain its one-archive
/// per library layout without compiling every bundled Arduino library.
pub fn resolve_framework_library_selection_active_declared(
    libraries: &[FrameworkLibrary],
    project_dir: &Path,
    src_dir: &Path,
    defines: &HashMap<String, String>,
    declared: &[String],
) -> fbuild_library_select::Selection {
    resolve_framework_library_selection_active_declared_with_extra(
        libraries,
        project_dir,
        src_dir,
        defines,
        declared,
        &[],
        &[],
    )
}

/// Active framework selection with additional translation-unit seeds and the
/// compiler's complete include path.
///
/// An external library can include a framework header from one of its own
/// `.cpp` files. The compiler sees that dependency, so the LDF must see it as
/// well or the selected framework archive is omitted from the final link.
/// The complete include path also lets the scanner resolve SDK headers that
/// define capability macros guarding later framework-library includes (for
/// example `soc/soc_caps.h` guarding ESP32's `LittleFS.h`).
pub fn resolve_framework_library_selection_active_declared_with_extra(
    libraries: &[FrameworkLibrary],
    project_dir: &Path,
    src_dir: &Path,
    defines: &HashMap<String, String>,
    declared: &[String],
    extra_source_files: &[PathBuf],
    compiler_include_dirs: &[PathBuf],
) -> fbuild_library_select::Selection {
    let roots = framework_include_scan_roots(project_dir, src_dir);
    let filtered = filter_framework_libs_shadowed_by_project(libraries, &roots);
    let mut seeds = collect_project_seeds(&roots, declared);
    seeds.extend_from_slice(extra_source_files);
    // Preserve the compiler's observable include order. In particular, ESP32
    // searches core/variant/SDK headers before project headers; reversing that
    // order can make the LDF inspect a shadowing header the compiler never
    // sees and derive the wrong capability set.
    let mut search_paths = Vec::new();
    for include_dir in compiler_include_dirs {
        push_existing_unique(&mut search_paths, include_dir.clone());
    }
    for project_path in project_search_paths(&roots) {
        push_existing_unique(&mut search_paths, project_path);
    }
    fbuild_library_select::resolve_with_stats_active_declared(
        &seeds,
        &search_paths,
        &filtered,
        defines,
        declared,
    )
    .0
}

/// Warn when a project sets `lib_ldf_mode`, which fbuild does not implement.
///
/// The resolver is fixed at a `chain`-style scan seeded from project sources.
/// Accepting the key silently lets a project believe `deep` is in effect and
/// spend a debugging session wondering why it changed nothing
/// (FastLED/fbuild#1214). `chain` and `off` are close enough to the actual
/// behavior to pass without noise.
pub fn warn_if_lib_ldf_mode_unsupported(mode: Option<&str>) {
    let Some(mode) = mode.map(str::trim).filter(|m| !m.is_empty()) else {
        return;
    };
    if mode.eq_ignore_ascii_case("chain") || mode.eq_ignore_ascii_case("off") {
        return;
    }
    tracing::warn!(
        lib_ldf_mode = %mode,
        "lib_ldf_mode is not implemented and has no effect; fbuild always \
         resolves libraries with a chain-style scan seeded from project \
         sources. Declare the dependency with `lib_deps` instead."
    );
}

/// Drop framework libraries whose primary header (`<lib_name>.h`) is
/// shadowed by a same-basename header anywhere under the supplied
/// `shadowing_roots`. See FastLED/fbuild#263.
///
/// Why this exists: the LDF resolver's path-prefix attribution can
/// mis-select a framework library when the user's own project also
/// owns that library's headers — even with the project's include
/// roots searched first, a transitive `#include` from the user's
/// header (e.g. `noise.h`) can resolve into the framework's bundled
/// copy if the project doesn't ship the transitive header itself.
/// That pulls the bundled library's `.cpp` files into the build set,
/// producing `multiple definition` link errors for every symbol that
/// exists in both copies.
///
/// The filter is intentionally conservative: it only drops a library
/// when the project itself ships a header matching the library's
/// canonical name. Other libraries are unaffected.
pub fn filter_framework_libs_shadowed_by_project(
    libraries: &[FrameworkLibrary],
    shadowing_roots: &[PathBuf],
) -> Vec<FrameworkLibrary> {
    let project_headers = collect_header_basenames(shadowing_roots);
    libraries
        .iter()
        .filter(|lib| {
            let primary = format!("{}.h", lib.name).to_lowercase();
            if project_headers.contains(&primary) {
                tracing::info!(
                    library = %lib.name,
                    "dropping framework library: shadowed by project header `{}.h` — see #263",
                    lib.name,
                );
                false
            } else {
                true
            }
        })
        .cloned()
        .collect()
}

/// Collect the lowercased basename of every project header that is
/// reachable as a bare `<basename>` include — i.e., a header that sits
/// at an include-root level the compiler would actually consult when
/// resolving `<SPI.h>`-style includes.
///
/// Why this is not a plain recursive walk: nested headers like
/// `lib/FastLED/fl/channels/spi.h` are includeable only as
/// `<fl/channels/spi.h>` (relative to the FastLED library's include
/// root), never as `<spi.h>`. A recursive walk would lowercase that
/// nested basename to `"spi.h"` and incorrectly mark the framework
/// `SPI` library as shadowed, dropping it from the link set and
/// causing `undefined reference to SPIClass::*` failures on Teensy 4.x.
/// See FastLED/fbuild#284.
///
/// Rules per Arduino library include resolution:
/// * For a `lib/` root (PIO library meta-directory), walk the top
///   level of each direct subdirectory plus that subdirectory's `src/`
///   (Arduino 1.5 layout). Headers deeper in the tree are skipped —
///   they can only be included via their full sub-path.
/// * For any other root (sketch dir, project `src/`, project
///   `include/`), walk only the root's top level.
fn collect_header_basenames(roots: &[PathBuf]) -> HashSet<String> {
    let mut out = HashSet::new();
    for root in roots {
        if !root.exists() {
            continue;
        }
        let is_lib_dir = root
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.eq_ignore_ascii_case("lib"))
            .unwrap_or(false);
        if is_lib_dir {
            let Ok(entries) = std::fs::read_dir(root) else {
                continue;
            };
            for entry in entries.flatten() {
                let dir = entry.path();
                if !dir.is_dir() {
                    continue;
                }
                let name = dir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default()
                    .to_lowercase();
                if matches!(
                    name.as_str(),
                    ".git"
                        | ".pio"
                        | fbuild_paths::FBUILD_DIR_NAME
                        | ".zap"
                        | ".build"
                        | "build"
                        | "target"
                ) {
                    continue;
                }
                collect_top_level_headers(&dir, &mut out);
                let src = dir.join("src");
                if src.is_dir() {
                    collect_top_level_headers(&src, &mut out);
                }
            }
        } else {
            collect_top_level_headers(root, &mut out);
        }
    }
    out
}

/// Insert the lowercased basename of every header file located directly
/// inside `dir` (non-recursive).
fn collect_top_level_headers(dir: &Path, out: &mut HashSet<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_lowercase();
        if matches!(ext.as_str(), "h" | "hh" | "hpp" | "hxx") {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                out.insert(name.to_lowercase());
            }
        }
    }
}

/// Walk project roots for source seeds, delegate to the LDF-style resolver,
/// and flatten the selection into the orchestrator-expected `Vec<PathBuf>`
/// of compile-set source files.
pub fn resolve_framework_library_sources_from_libraries(
    libraries: &[FrameworkLibrary],
    roots: &[PathBuf],
) -> Vec<PathBuf> {
    if libraries.is_empty() {
        return Vec::new();
    }

    let seeds = collect_project_seeds(roots, &[]);
    let search_paths = project_search_paths(roots);
    let selection = resolve_library_selection(&seeds, &search_paths, libraries);

    for name in &selection.required_libraries {
        if let Some(lib) = libraries.iter().find(|l| &l.name == name) {
            tracing::info!(
                "selected framework library '{}': {} source files",
                lib.name,
                lib.source_files.len()
            );
        }
    }

    selection.source_files
}

/// Cached counterpart to [`resolve_framework_library_sources`].
///
/// Routes the same `(libraries, project_dir, src_dir)` resolution through
/// `fbuild_library_select::cache::resolve_cached` using the supplied
/// `FileKvStore`. On a backend failure (open, read, write) we log a warning and
/// fall back to the uncached `resolve(...)` so a degraded cache can never
/// poison a build — same philosophy as the corrupt-entry handling already
/// inside `cache.rs`.
pub fn resolve_framework_library_sources_cached(
    libraries: &[FrameworkLibrary],
    project_dir: &Path,
    src_dir: &Path,
    key_inputs: &CacheKeyInputs<'_>,
    store: &FileKvStore,
) -> Vec<PathBuf> {
    let (sources, _hit) = resolve_framework_library_sources_cached_with_hit(
        libraries,
        project_dir,
        src_dir,
        key_inputs,
        store,
    );
    sources
}

/// Internal helper that returns `(sources, from_cache)` so tests can assert
/// hit/miss without the public API surfacing that bit. The hit flag is
/// `false` whenever the cache backend errored and we fell back to the
/// uncached resolver.
pub(crate) fn resolve_framework_library_sources_cached_with_hit(
    libraries: &[FrameworkLibrary],
    project_dir: &Path,
    src_dir: &Path,
    key_inputs: &CacheKeyInputs<'_>,
    store: &FileKvStore,
) -> (Vec<PathBuf>, bool) {
    let roots = framework_include_scan_roots(project_dir, src_dir);
    if libraries.is_empty() {
        return (Vec::new(), false);
    }

    // Defensive filter: drop framework libraries whose primary header
    // is shadowed by a project-owned header. See #263.
    let filtered = filter_framework_libs_shadowed_by_project(libraries, &roots);
    if filtered.is_empty() {
        return (Vec::new(), false);
    }

    let seeds = collect_project_seeds(&roots, key_inputs.declared_deps);
    let search_paths = project_search_paths(&roots);

    match resolve_cached(&seeds, &search_paths, &filtered, key_inputs, store) {
        Ok(cached) => {
            for name in &cached.selection.required_libraries {
                if let Some(lib) = filtered.iter().find(|l| &l.name == name) {
                    tracing::info!(
                        "selected framework library '{}': {} source files",
                        lib.name,
                        lib.source_files.len()
                    );
                }
            }
            tracing::info!(
                cache = if cached.from_cache { "hit" } else { "miss" },
                key = %cached.key.to_hex(),
                "library-select cache: {}",
                if cached.from_cache { "hit" } else { "miss" }
            );
            (cached.selection.source_files, cached.from_cache)
        }
        Err(err) => {
            tracing::warn!(
                error = %err,
                "library-select cache backend error; falling back to uncached resolve"
            );
            (
                resolve_framework_library_sources_active(
                    &filtered,
                    project_dir,
                    src_dir,
                    key_inputs.preprocessor_defines,
                ),
                false,
            )
        }
    }
}

/// Process-shared file store for the library-selection cache.
///
/// Opens lazily on first call and caches the handle for the rest of the
/// process. Returns `None` on open failure — callers must skip caching
/// (and route through the uncached resolver) rather than crash.
pub fn library_select_kv_store() -> Option<&'static FileKvStore> {
    static STORE: OnceLock<Option<FileKvStore>> = OnceLock::new();
    STORE
        .get_or_init(|| {
            let dir = library_select_cache_dir();
            match FileKvStore::open(&dir) {
                Ok(store) => {
                    tracing::info!(
                        path = %dir.display(),
                        "library-select cache: opened file store"
                    );
                    Some(store)
                }
                Err(err) => {
                    tracing::warn!(
                        path = %dir.display(),
                        error = %err,
                        "library-select cache: failed to open file store; \
                         resolution will run uncached"
                    );
                    None
                }
            }
        })
        .as_ref()
}

/// Filesystem location of the library-selection file store.
///
/// Routes through `fbuild_paths::get_cache_root()` so the cache obeys the
/// dev/prod isolation contract (`FBUILD_DEV_MODE=1` → `~/.fbuild/dev/cache`)
/// and any `FBUILD_CACHE_DIR` override.
fn library_select_cache_dir() -> PathBuf {
    fbuild_paths::get_cache_root().join("library-selection")
}

/// Project directories to scan for `#include` directives and local headers.
pub fn framework_include_scan_roots(project_dir: &Path, src_dir: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    push_existing_unique(&mut roots, src_dir.to_path_buf());
    push_existing_unique(&mut roots, project_dir.join("src"));
    push_existing_unique(&mut roots, project_dir.join("include"));
    push_existing_unique(&mut roots, project_dir.join("lib"));
    roots
}

fn push_existing_unique(roots: &mut Vec<PathBuf>, path: PathBuf) {
    if !path.exists() {
        return;
    }
    if !roots.iter().any(|existing| existing == &path) {
        roots.push(path);
    }
}

/// Include search paths for the project and its local Arduino libraries.
///
/// Local libraries live under `lib/<name>/` (or `lib/<name>/src/`), but the
/// `lib/` directory itself cannot resolve `<FastLED.h>`. Add each library's
/// public root while retaining the project roots ahead of framework libraries.
fn project_search_paths(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut paths = roots.to_vec();
    for root in roots {
        if !is_library_root(root) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            push_existing_unique(&mut paths, dir.clone());
            push_existing_unique(&mut paths, dir.join("src"));
        }
    }
    paths
}

/// Local `lib/` libraries the project's include graph reaches, plus any named
/// in `lib_deps` (FastLED/fbuild#1410).
///
/// PlatformIO's LDF compiles a `lib/` library only when something includes
/// it. fbuild compiled every one, so a SAMD-only library sitting in `lib/`
/// failed an AVR build whose sketch never included it. The walk starts at the
/// project's translation units and follows library-to-library includes, so a
/// library reached only through another local library is still selected.
///
/// The scan is textual — every `#if` arm — on purpose. The active scan treats
/// a compiler-builtin macro (`__XTENSA__`, `__AVR__`) as defined nowhere, so
/// it would prune a guarded include the compiler does take and drop a library
/// the link needs. Scanning every arm can only over-select, which is the
/// behavior every local library had before this.
pub fn select_local_libraries(
    project_dir: &Path,
    src_dir: &Path,
    declared: &[String],
) -> Vec<FrameworkLibrary> {
    select_local_libraries_in(
        &framework_include_scan_roots(project_dir, src_dir),
        declared,
    )
}

fn select_local_libraries_in(roots: &[PathBuf], declared: &[String]) -> Vec<FrameworkLibrary> {
    let libraries: Vec<FrameworkLibrary> = roots
        .iter()
        .filter(|root| is_library_root(root))
        .flat_map(|root| discover_local_libraries(root))
        .collect();
    if libraries.is_empty() {
        return libraries;
    }
    let selection = fbuild_library_select::resolve_declared(
        &collect_sketch_seeds(roots),
        &project_search_paths(roots),
        &libraries,
        declared,
    );
    libraries
        .into_iter()
        .filter(|library| {
            let used = selection.required_libraries.contains(&library.name);
            if !used {
                tracing::info!(
                    library = %library.name,
                    "skipping local library: no project source includes it and lib_deps does not name it"
                );
            }
            used
        })
        .collect()
}

/// Every library directory under a `lib/` root, described by what the build
/// compiles for it ([`InstalledLibrary::get_source_files`]).
///
/// The library root is always an attribution dir, so a header anywhere in the
/// library — not only under the `src/` the compiler searches first — counts as
/// reaching it.
///
/// [`InstalledLibrary::get_source_files`]: fbuild_packages::library::library_info::InstalledLibrary::get_source_files
fn discover_local_libraries(lib_root: &Path) -> Vec<FrameworkLibrary> {
    let Ok(entries) = std::fs::read_dir(lib_root) else {
        return Vec::new();
    };
    let mut libraries = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let mut include_dirs =
            fbuild_packages::library::framework_library::library_include_dirs(&dir);
        if !include_dirs.contains(&dir) {
            include_dirs.push(dir.clone());
        }
        let source_files =
            fbuild_packages::library::library_info::InstalledLibrary::new(&dir, &name)
                .get_source_files();
        libraries.push(FrameworkLibrary {
            name,
            dir,
            include_dirs,
            source_files,
        });
    }
    libraries.sort_by(|a, b| a.name.cmp(&b.name));
    libraries
}

/// Collect translation units as walker seeds.
///
/// Headers are never seeds: they must be reached through some TU's include
/// graph, or an inactive header anywhere under `lib/` turns into a false
/// framework-library dependency (FastLED/fbuild#1094).
///
/// Translation units of a local library the build compiles *are* seeds. A
/// local library's `.cpp` files are compiled and linked, so an include one of
/// them makes is a real dependency — FastLED expresses its Adafruit_NeoPixel
/// and Audio dependencies exactly there, and seeding only the sketch meant
/// those libraries were on the include path but never on the link line,
/// failing all eight Teensy boards with `undefined reference`
/// (FastLED/fbuild#1337, the #1214 class).
///
/// The invariant is *"what compiles is what seeds"*: the scanner's view of the
/// build has to match the compiler's, or the two disagree about a dependency
/// and the link breaks. That is why only the local libraries
/// [`select_local_libraries`] picks seed — an unselected one is not compiled
/// (FastLED/fbuild#1410).
fn collect_project_seeds(roots: &[PathBuf], declared: &[String]) -> Vec<PathBuf> {
    let mut seeds = collect_sketch_seeds(roots);
    for library in select_local_libraries_in(roots, declared) {
        seeds.extend(library.source_files);
    }
    seeds
}

/// Translation units outside every `lib/` root. The project directory itself
/// is a root when the sketch has no `src/`, so `lib/` is pruned from the walk
/// rather than left to [`is_library_root`].
fn collect_sketch_seeds(roots: &[PathBuf]) -> Vec<PathBuf> {
    let lib_roots: Vec<&PathBuf> = roots.iter().filter(|r| is_library_root(r)).collect();
    let mut seeds = Vec::new();
    for root in roots {
        if !root.exists() || is_library_root(root) {
            continue;
        }
        for entry in WalkDir::new(root)
            .into_iter()
            .filter_entry(|e| {
                should_scan_entry(e) && !lib_roots.iter().any(|l| e.path() == l.as_path())
            })
            .flatten()
        {
            if entry.file_type().is_file() && is_translation_unit(entry.path()) {
                seeds.push(entry.path().to_path_buf());
            }
        }
    }
    seeds
}

fn is_library_root(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.eq_ignore_ascii_case("lib"))
        .unwrap_or(false)
}

fn should_scan_entry(entry: &DirEntry) -> bool {
    let name = entry.file_name().to_string_lossy().to_lowercase();
    !matches!(
        name.as_str(),
        ".git"
            | ".pio"
            | fbuild_paths::FBUILD_DIR_NAME
            | ".zap"
            | ".build"
            | "build"
            | "target"
            | ".venv"
            | "venv"
            | "node_modules"
            | "__pycache__"
    )
}

fn is_translation_unit(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_lowercase();
    matches!(ext.as_str(), "c" | "cpp" | "cc" | "cxx" | "s" | "ino")
}

#[cfg(test)]
#[path = "framework_libs_tests.rs"]
mod tests;
