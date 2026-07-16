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
use fbuild_library_select::{
    resolve as resolve_library_selection, resolve_active as resolve_active_library_selection,
};
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
    let roots = framework_include_scan_roots(project_dir, src_dir);
    let filtered = filter_framework_libs_shadowed_by_project(libraries, &roots);
    let seeds = collect_project_seeds(&roots);
    let search_paths = project_search_paths(&roots);
    resolve_active_library_selection(&seeds, &search_paths, &filtered, defines).source_files
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

/// Collect translation units as walker seeds. Headers must be reached through
/// the sketch's transitive include graph; scanning every header under `lib/`
/// turns inactive library code into false framework-library dependencies.
fn collect_project_seeds(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut seeds = Vec::new();
    for root in roots {
        if !root.exists() || is_library_root(root) {
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
mod tests {
    use super::*;

    #[test]
    fn resolves_libraries_from_project_includes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_src = tmp.path().join("project").join("src");
        std::fs::create_dir_all(&project_src).unwrap();
        std::fs::write(
            project_src.join("main.cpp"),
            "#include <SPI.h>\n#include <OctoWS2811.h>\n",
        )
        .unwrap();

        let spi_dir = tmp.path().join("framework").join("libraries").join("SPI");
        std::fs::create_dir_all(&spi_dir).unwrap();
        std::fs::write(spi_dir.join("SPI.h"), "").unwrap();
        std::fs::write(spi_dir.join("SPI.cpp"), "").unwrap();

        let octo_dir = tmp
            .path()
            .join("framework")
            .join("libraries")
            .join("OctoWS2811");
        std::fs::create_dir_all(&octo_dir).unwrap();
        std::fs::write(octo_dir.join("OctoWS2811.h"), "").unwrap();
        std::fs::write(octo_dir.join("OctoWS2811.cpp"), "").unwrap();
        std::fs::write(octo_dir.join("OctoWS2811_imxrt.cpp"), "").unwrap();

        let libraries = vec![
            FrameworkLibrary {
                name: "OctoWS2811".to_string(),
                dir: octo_dir.clone(),
                include_dirs: vec![octo_dir.clone()],
                source_files: vec![
                    octo_dir.join("OctoWS2811.cpp"),
                    octo_dir.join("OctoWS2811_imxrt.cpp"),
                ],
            },
            FrameworkLibrary {
                name: "SPI".to_string(),
                dir: spi_dir.clone(),
                include_dirs: vec![spi_dir.clone()],
                source_files: vec![spi_dir.join("SPI.cpp")],
            },
        ];

        let mut sources = resolve_framework_library_sources_from_libraries(
            &libraries,
            std::slice::from_ref(&project_src),
        );
        sources.sort();

        let mut expected = vec![
            octo_dir.join("OctoWS2811.cpp"),
            octo_dir.join("OctoWS2811_imxrt.cpp"),
            spi_dir.join("SPI.cpp"),
        ];
        expected.sort();
        assert_eq!(sources, expected);
    }

    #[test]
    fn follows_transitive_includes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_src = tmp.path().join("project").join("src");
        std::fs::create_dir_all(&project_src).unwrap();
        std::fs::write(project_src.join("main.cpp"), "#include <NeedsSpi.h>\n").unwrap();

        let spi_dir = tmp.path().join("framework").join("libraries").join("SPI");
        std::fs::create_dir_all(&spi_dir).unwrap();
        std::fs::write(spi_dir.join("SPI.h"), "").unwrap();
        std::fs::write(spi_dir.join("SPI.cpp"), "").unwrap();

        let wrapper_dir = tmp
            .path()
            .join("framework")
            .join("libraries")
            .join("NeedsSpi");
        std::fs::create_dir_all(&wrapper_dir).unwrap();
        std::fs::write(wrapper_dir.join("NeedsSpi.h"), "#include <SPI.h>\n").unwrap();
        std::fs::write(wrapper_dir.join("NeedsSpi.cpp"), "").unwrap();

        let libraries = vec![
            FrameworkLibrary {
                name: "NeedsSpi".to_string(),
                dir: wrapper_dir.clone(),
                include_dirs: vec![wrapper_dir.clone()],
                source_files: vec![wrapper_dir.join("NeedsSpi.cpp")],
            },
            FrameworkLibrary {
                name: "SPI".to_string(),
                dir: spi_dir.clone(),
                include_dirs: vec![spi_dir.clone()],
                source_files: vec![spi_dir.join("SPI.cpp")],
            },
        ];

        let mut sources = resolve_framework_library_sources_from_libraries(
            &libraries,
            std::slice::from_ref(&project_src),
        );
        sources.sort();

        let mut expected = vec![wrapper_dir.join("NeedsSpi.cpp"), spi_dir.join("SPI.cpp")];
        expected.sort();
        assert_eq!(sources, expected);
    }

    #[test]
    fn unrelated_library_not_selected() {
        // Regression guard for #204: libraries whose headers are never
        // referenced must not appear in the compile set.
        let tmp = tempfile::TempDir::new().unwrap();
        let project_src = tmp.path().join("project").join("src");
        std::fs::create_dir_all(&project_src).unwrap();
        std::fs::write(project_src.join("main.cpp"), "#include <SPI.h>\n").unwrap();

        let spi_dir = tmp.path().join("framework").join("libraries").join("SPI");
        std::fs::create_dir_all(&spi_dir).unwrap();
        std::fs::write(spi_dir.join("SPI.h"), "").unwrap();
        std::fs::write(spi_dir.join("SPI.cpp"), "").unwrap();

        let fnet_dir = tmp.path().join("framework").join("libraries").join("FNET");
        std::fs::create_dir_all(&fnet_dir).unwrap();
        std::fs::write(fnet_dir.join("fnet.h"), "").unwrap();
        std::fs::write(fnet_dir.join("fnet.cpp"), "").unwrap();

        let libraries = vec![
            FrameworkLibrary {
                name: "FNET".to_string(),
                dir: fnet_dir.clone(),
                include_dirs: vec![fnet_dir.clone()],
                source_files: vec![fnet_dir.join("fnet.cpp")],
            },
            FrameworkLibrary {
                name: "SPI".to_string(),
                dir: spi_dir.clone(),
                include_dirs: vec![spi_dir.clone()],
                source_files: vec![spi_dir.join("SPI.cpp")],
            },
        ];

        let sources = resolve_framework_library_sources_from_libraries(
            &libraries,
            std::slice::from_ref(&project_src),
        );
        assert_eq!(sources, vec![spi_dir.join("SPI.cpp")]);
    }

    #[test]
    fn inactive_local_library_header_cannot_select_framework_library() {
        // FastLED/fbuild#1094: a header anywhere under project lib/ used to
        // become an independent seed. Its inactive include then selected a
        // framework library even though the sketch could not reach it.
        let tmp = tempfile::TempDir::new().unwrap();
        let project_src = tmp.path().join("project").join("src");
        let project_lib = tmp.path().join("project").join("lib");
        let fastled = project_lib.join("FastLED");
        std::fs::create_dir_all(&project_src).unwrap();
        std::fs::create_dir_all(&fastled).unwrap();
        std::fs::write(project_src.join("main.cpp"), "#include <FastLED.h>\n").unwrap();
        std::fs::write(fastled.join("FastLED.h"), "#include <SPI.h>\n").unwrap();
        std::fs::write(fastled.join("inactive_audio.h"), "#include <Audio.h>\n").unwrap();

        let spi_dir = tmp.path().join("framework").join("libraries").join("SPI");
        std::fs::create_dir_all(&spi_dir).unwrap();
        std::fs::write(spi_dir.join("SPI.h"), "").unwrap();
        std::fs::write(spi_dir.join("SPI.cpp"), "").unwrap();

        let audio_dir = tmp.path().join("framework").join("libraries").join("Audio");
        std::fs::create_dir_all(&audio_dir).unwrap();
        std::fs::write(audio_dir.join("Audio.h"), "").unwrap();
        std::fs::write(audio_dir.join("Audio.cpp"), "").unwrap();

        let libraries = vec![
            FrameworkLibrary {
                name: "Audio".to_string(),
                dir: audio_dir.clone(),
                include_dirs: vec![audio_dir.clone()],
                source_files: vec![audio_dir.join("Audio.cpp")],
            },
            FrameworkLibrary {
                name: "SPI".to_string(),
                dir: spi_dir.clone(),
                include_dirs: vec![spi_dir.clone()],
                source_files: vec![spi_dir.join("SPI.cpp")],
            },
        ];

        let sources = resolve_framework_library_sources_from_libraries(
            &libraries,
            &[project_src, project_lib],
        );
        assert_eq!(sources, vec![spi_dir.join("SPI.cpp")]);
    }

    #[test]
    fn prefers_local_library_over_framework() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_src = tmp.path().join("project").join("src");
        let project_lib = tmp
            .path()
            .join("project")
            .join("lib")
            .join("FastLED")
            .join("src");
        std::fs::create_dir_all(&project_src).unwrap();
        std::fs::create_dir_all(&project_lib).unwrap();
        std::fs::write(project_src.join("main.cpp"), "#include <FastLED.h>\n").unwrap();
        std::fs::write(project_lib.join("FastLED.h"), "#include <SPI.h>\n").unwrap();
        std::fs::write(project_lib.join("FastLED.cpp"), "").unwrap();

        let framework_fastled_dir = tmp
            .path()
            .join("framework")
            .join("libraries")
            .join("FastLED");
        std::fs::create_dir_all(&framework_fastled_dir).unwrap();
        std::fs::write(framework_fastled_dir.join("FastLED.h"), "").unwrap();
        std::fs::write(framework_fastled_dir.join("FastLED.cpp"), "").unwrap();

        let spi_dir = tmp.path().join("framework").join("libraries").join("SPI");
        std::fs::create_dir_all(&spi_dir).unwrap();
        std::fs::write(spi_dir.join("SPI.h"), "").unwrap();
        std::fs::write(spi_dir.join("SPI.cpp"), "").unwrap();

        let libraries = vec![
            FrameworkLibrary {
                name: "FastLED".to_string(),
                dir: framework_fastled_dir.clone(),
                include_dirs: vec![framework_fastled_dir.clone()],
                source_files: vec![framework_fastled_dir.join("FastLED.cpp")],
            },
            FrameworkLibrary {
                name: "SPI".to_string(),
                dir: spi_dir.clone(),
                include_dirs: vec![spi_dir.clone()],
                source_files: vec![spi_dir.join("SPI.cpp")],
            },
        ];

        let roots = vec![project_src, project_lib];
        let sources = resolve_framework_library_sources_from_libraries(&libraries, &roots);

        assert_eq!(sources, vec![spi_dir.join("SPI.cpp")]);
    }

    /// Regression for FastLED/fbuild#263 — case A: when the user's project
    /// IS the library (FastLED's own source tree has `src/FastLED.h`
    /// directly under one of the walker's roots), the framework's bundled
    /// FastLED at `cores/teensy4/libraries/FastLED/` must not get selected.
    /// This case works in the LDF resolver today because path-prefix
    /// attribution finds `project/src/FastLED.h` first.
    #[test]
    fn project_is_the_library_does_not_pull_in_bundled_copy() {
        let tmp = tempfile::TempDir::new().unwrap();

        let project_src = tmp.path().join("project").join("src");
        std::fs::create_dir_all(&project_src).unwrap();
        std::fs::write(project_src.join("FastLED.h"), "// the real FastLED\n").unwrap();
        std::fs::write(project_src.join("FastLED.cpp"), "// user impl\n").unwrap();
        std::fs::write(
            project_src.join("example_main.cpp"),
            "#include <FastLED.h>\n",
        )
        .unwrap();

        let bundled_fastled_dir = tmp
            .path()
            .join("framework")
            .join("libraries")
            .join("FastLED");
        std::fs::create_dir_all(&bundled_fastled_dir).unwrap();
        std::fs::write(
            bundled_fastled_dir.join("FastLED.h"),
            "// bundled (stale) FastLED\n",
        )
        .unwrap();
        std::fs::write(bundled_fastled_dir.join("FastLED.cpp"), "// bundled impl\n").unwrap();

        let libraries = vec![FrameworkLibrary {
            name: "FastLED".to_string(),
            dir: bundled_fastled_dir.clone(),
            include_dirs: vec![bundled_fastled_dir.clone()],
            source_files: vec![bundled_fastled_dir.join("FastLED.cpp")],
        }];

        let sources = resolve_framework_library_sources_from_libraries(
            &libraries,
            std::slice::from_ref(&project_src),
        );

        assert!(
            sources.is_empty(),
            "bundled FastLED must NOT be selected when the project owns FastLED.h \
             directly under src/ — see #263. Got: {sources:?}"
        );
    }

    /// Regression for FastLED/fbuild#263 — case B: the user's project owns
    /// FastLED.h at a path that is NOT one of the walker roots passed to
    /// the resolver (e.g. `<repo>/src/FastLED.h` while the resolver only
    /// sees `<repo>/tests/platform/teensy41/src/`). The walker then can
    /// only find FastLED.h via the framework's bundled
    /// `cores/teensy4/libraries/FastLED/` include dir, mis-attributes the
    /// include to the bundled library, and pulls its sources into the
    /// build set — duplicate-symbol time. The fix in `framework_libs.rs`
    /// drops framework libraries whose primary header is shadowed by a
    /// project header even when the project header isn't first in the
    /// search order.
    #[test]
    fn example_only_root_does_not_pull_in_bundled_fastled_when_user_owns_fastled() {
        let tmp = tempfile::TempDir::new().unwrap();

        // The repo: user's local FastLED lives at <repo>/src/, which is
        // NOT among the resolver's roots for the per-example build.
        let repo_src = tmp.path().join("repo").join("src");
        std::fs::create_dir_all(&repo_src).unwrap();
        std::fs::write(repo_src.join("FastLED.h"), "// the real FastLED\n").unwrap();
        std::fs::write(repo_src.join("FastLED.cpp"), "// user impl\n").unwrap();

        // The per-example project root the resolver actually sees.
        let example_src = tmp
            .path()
            .join("repo")
            .join("tests")
            .join("platform")
            .join("teensy41")
            .join("src");
        std::fs::create_dir_all(&example_src).unwrap();
        std::fs::write(
            example_src.join("example_main.cpp"),
            "#include <FastLED.h>\n",
        )
        .unwrap();

        // Framework bundles its own FastLED.
        let bundled_fastled_dir = tmp
            .path()
            .join("framework")
            .join("libraries")
            .join("FastLED");
        std::fs::create_dir_all(&bundled_fastled_dir).unwrap();
        std::fs::write(bundled_fastled_dir.join("FastLED.h"), "// bundled\n").unwrap();
        std::fs::write(bundled_fastled_dir.join("FastLED.cpp"), "// bundled impl\n").unwrap();

        let libraries = vec![FrameworkLibrary {
            name: "FastLED".to_string(),
            dir: bundled_fastled_dir.clone(),
            include_dirs: vec![bundled_fastled_dir.clone()],
            source_files: vec![bundled_fastled_dir.join("FastLED.cpp")],
        }];

        // The fbuild build pipeline calls `local_overridden_framework_libs`
        // with both the example root AND the repo's actual src/ as
        // shadowing roots. The repo src/FastLED.h shadows the framework's
        // FastLED → framework library is filtered out before the resolver
        // ever sees it.
        let shadowing_roots = vec![example_src.clone(), repo_src.clone()];
        let filtered = filter_framework_libs_shadowed_by_project(&libraries, &shadowing_roots);

        // Resolver runs on the FILTERED library set.
        let sources = resolve_framework_library_sources_from_libraries(
            &filtered,
            std::slice::from_ref(&example_src),
        );

        assert!(
            sources.is_empty(),
            "bundled FastLED must be filtered out because the user's repo owns \
             FastLED.h even when it's not in the per-example walker roots — see #263. \
             Got: {sources:?}"
        );
    }

    /// Regression for FastLED/fbuild#284 — a nested project header whose
    /// basename happens to collide with a framework library's primary
    /// header must NOT trigger the shadowing filter. FastLED ships
    /// `lib/FastLED/fl/channels/spi.h`, which is includeable only as
    /// `<fl/channels/spi.h>`, never as `<SPI.h>`. The framework's `SPI`
    /// library must therefore stay in the build set, otherwise every
    /// Teensy 4.x example fails at link with `undefined reference to
    /// SPIClass::*`.
    ///
    /// At the same time, the existing `#263` behaviour for headers
    /// reachable as bare `<basename>` (e.g. `lib/FastLED/noise.h` or
    /// `project/src/FastLED.h`) must still drop the matching framework
    /// library.
    #[test]
    fn nested_basename_does_not_shadow_framework_library() {
        let tmp = tempfile::TempDir::new().unwrap();

        // PIO project layout: lib/FastLED/ contains FastLED's source
        // tree directly (1.0 flat layout — no src/ subdir). spi.h is
        // nested deep, noise.h sits at FastLED's include root.
        let project_dir = tmp.path().join("project");
        let lib_dir = project_dir.join("lib");
        let fastled_dir = lib_dir.join("FastLED");
        let nested_spi_dir = fastled_dir.join("fl").join("channels");
        std::fs::create_dir_all(&nested_spi_dir).unwrap();
        std::fs::write(nested_spi_dir.join("spi.h"), "// FastLED internal\n").unwrap();
        std::fs::write(fastled_dir.join("FastLED.h"), "").unwrap();
        std::fs::write(fastled_dir.join("noise.h"), "// shadows framework Noise\n").unwrap();

        let src_dir = project_dir.join("src");
        std::fs::create_dir_all(&src_dir).unwrap();

        // Framework libs: SPI (must SURVIVE the filter) and Noise (must
        // be dropped because the project owns noise.h at the FastLED
        // library include root).
        let spi_dir = tmp.path().join("framework").join("libraries").join("SPI");
        std::fs::create_dir_all(&spi_dir).unwrap();
        std::fs::write(spi_dir.join("SPI.h"), "").unwrap();
        std::fs::write(spi_dir.join("SPI.cpp"), "").unwrap();

        let noise_dir = tmp.path().join("framework").join("libraries").join("Noise");
        std::fs::create_dir_all(&noise_dir).unwrap();
        std::fs::write(noise_dir.join("noise.h"), "").unwrap();
        std::fs::write(noise_dir.join("noise.cpp"), "").unwrap();

        let libraries = vec![
            FrameworkLibrary {
                name: "Noise".to_string(),
                dir: noise_dir.clone(),
                include_dirs: vec![noise_dir.clone()],
                source_files: vec![noise_dir.join("noise.cpp")],
            },
            FrameworkLibrary {
                name: "SPI".to_string(),
                dir: spi_dir.clone(),
                include_dirs: vec![spi_dir.clone()],
                source_files: vec![spi_dir.join("SPI.cpp")],
            },
        ];

        let shadowing_roots = framework_include_scan_roots(&project_dir, &src_dir);
        let filtered = filter_framework_libs_shadowed_by_project(&libraries, &shadowing_roots);

        let surviving: Vec<&str> = filtered.iter().map(|l| l.name.as_str()).collect();
        assert!(
            surviving.contains(&"SPI"),
            "framework SPI must SURVIVE — nested fl/channels/spi.h is not reachable \
             as <SPI.h> and must not trigger the shadowing filter — see #284. \
             Surviving libraries: {surviving:?}"
        );
        assert!(
            !surviving.contains(&"Noise"),
            "framework Noise must be dropped — lib/FastLED/noise.h sits at the \
             FastLED library include root and is reachable as <noise.h> — see #263. \
             Surviving libraries: {surviving:?}"
        );
    }

    #[test]
    fn cached_resolution_round_trips_through_file_store() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path().join("project");
        let src_dir = project_dir.join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(src_dir.join("main.cpp"), "#include <SPI.h>\n").unwrap();

        let spi_dir = tmp.path().join("framework").join("libraries").join("SPI");
        std::fs::create_dir_all(&spi_dir).unwrap();
        std::fs::write(spi_dir.join("SPI.h"), "").unwrap();
        std::fs::write(spi_dir.join("SPI.cpp"), "").unwrap();

        let libraries = vec![FrameworkLibrary {
            name: "SPI".to_string(),
            dir: spi_dir.clone(),
            include_dirs: vec![spi_dir.clone()],
            source_files: vec![spi_dir.join("SPI.cpp")],
        }];

        let framework_root = tmp.path().join("framework");
        let defines = HashMap::new();
        let key_inputs = CacheKeyInputs {
            toolchain_triple: "test-arm-none-eabi",
            framework_install_path: &framework_root,
            framework_version: "0.0.0-test",
            preprocessor_defines: &defines,
        };

        let kv = FileKvStore::open(tmp.path().join("kv")).unwrap();

        let (first, hit_first) = resolve_framework_library_sources_cached_with_hit(
            &libraries,
            &project_dir,
            &src_dir,
            &key_inputs,
            &kv,
        );
        assert!(!hit_first, "first call must miss the cache");
        assert_eq!(first, vec![spi_dir.join("SPI.cpp")]);

        let (second, hit_second) = resolve_framework_library_sources_cached_with_hit(
            &libraries,
            &project_dir,
            &src_dir,
            &key_inputs,
            &kv,
        );
        assert!(hit_second, "second call must hit the cache");
        assert_eq!(first, second, "cache hit must yield identical sources");
    }
}
