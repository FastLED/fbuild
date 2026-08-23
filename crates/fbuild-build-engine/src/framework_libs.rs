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
    let roots = framework_include_scan_roots(project_dir, src_dir);
    let filtered = filter_framework_libs_shadowed_by_project(libraries, &roots);
    let seeds = collect_project_seeds(&roots);
    let search_paths = project_search_paths(&roots);
    fbuild_library_select::resolve_with_stats_active_declared(
        &seeds,
        &search_paths,
        &filtered,
        defines,
        declared,
    )
    .0
    .source_files
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
                    ".git" | ".pio" | ".fbuild" | ".zap" | ".build" | "build" | "target"
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

    let seeds = collect_project_seeds(roots);
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

    let seeds = collect_project_seeds(&roots);
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

/// Collect translation units as walker seeds.
///
/// Headers are never seeds: they must be reached through some TU's include
/// graph, or an inactive header anywhere under `lib/` turns into a false
/// framework-library dependency (FastLED/fbuild#1094).
///
/// Translation units under `lib/` *are* seeds, though, and that is a change
/// from the original sketch-only rule. A local library's `.cpp` files are
/// compiled and linked, so an include one of them makes is a real dependency
/// — FastLED expresses its Adafruit_NeoPixel and Audio dependencies exactly
/// there, and seeding only the sketch meant those libraries were on the
/// include path but never on the link line, failing all eight Teensy boards
/// with `undefined reference` (FastLED/fbuild#1337, the #1214 class).
///
/// The invariant that replaces "sketch only" is *"what compiles is what
/// seeds"*: the scanner's view of the build has to match the compiler's, or
/// the two disagree about a dependency and the link breaks.
fn collect_project_seeds(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut seeds = Vec::new();
    for root in roots {
        if !root.exists() {
            continue;
        }
        if is_library_root(root) {
            collect_local_library_seeds(root, &mut seeds);
            continue;
        }
        for entry in WalkDir::new(root)
            .into_iter()
            .filter_entry(should_scan_entry)
            .flatten()
        {
            if !entry.file_type().is_file() {
                continue;
            }
            if is_translation_unit(entry.path()) {
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

/// Seed from the translation units a local library actually compiles.
///
/// Layout matters here, and getting it wrong breaks the invariant in the
/// other direction. An Arduino 1.5 library keeps its sources in `src/`; a 1.0
/// library keeps them at the library root. Either way `examples/`, `extras/`
/// and test trees are **not** compiled — seeding an example sketch would make
/// the scanner claim dependencies the build never links, which is the same
/// disagreement #1337 is about, mirrored.
fn collect_local_library_seeds(lib_root: &Path, seeds: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(lib_root) else {
        return;
    };
    for entry in entries.flatten() {
        let library_dir = entry.path();
        if !library_dir.is_dir() {
            continue;
        }
        let src = library_dir.join("src");
        let scan_root = if src.is_dir() { src } else { library_dir };
        for found in WalkDir::new(&scan_root)
            .into_iter()
            .filter_entry(should_scan_library_entry)
            .flatten()
        {
            if found.file_type().is_file() && is_translation_unit(found.path()) {
                seeds.push(found.path().to_path_buf());
            }
        }
    }
}

/// [`should_scan_entry`] plus the directories a library ships but never
/// compiles.
fn should_scan_library_entry(entry: &DirEntry) -> bool {
    if !should_scan_entry(entry) {
        return false;
    }
    let name = entry.file_name().to_string_lossy().to_lowercase();
    !matches!(
        name.as_str(),
        "examples" | "example" | "extras" | "test" | "tests" | "docs"
    )
}

fn should_scan_entry(entry: &DirEntry) -> bool {
    let name = entry.file_name().to_string_lossy().to_lowercase();
    !matches!(
        name.as_str(),
        ".git"
            | ".pio"
            | ".fbuild"
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
