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

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use fbuild_library_select::cache::{resolve_cached, CacheKeyInputs};
use fbuild_library_select::resolve as resolve_library_selection;
use fbuild_packages::library::FrameworkLibrary;
use walkdir::{DirEntry, WalkDir};
use zccache_artifact::KvStore;

/// Resolve framework library source files needed by a project.
pub fn resolve_framework_library_sources(
    libraries: &[FrameworkLibrary],
    project_dir: &Path,
    src_dir: &Path,
) -> Vec<PathBuf> {
    let roots = framework_include_scan_roots(project_dir, src_dir);
    resolve_framework_library_sources_from_libraries(libraries, &roots)
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
    let search_paths: Vec<PathBuf> = roots.to_vec();
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
/// `KvStore`. On a backend failure (open, read, write) we log a warning and
/// fall back to the uncached `resolve(...)` so a degraded cache can never
/// poison a build — same philosophy as the corrupt-entry handling already
/// inside `cache.rs`.
pub fn resolve_framework_library_sources_cached(
    libraries: &[FrameworkLibrary],
    project_dir: &Path,
    src_dir: &Path,
    key_inputs: &CacheKeyInputs<'_>,
    store: &KvStore,
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
    store: &KvStore,
) -> (Vec<PathBuf>, bool) {
    let roots = framework_include_scan_roots(project_dir, src_dir);
    if libraries.is_empty() {
        return (Vec::new(), false);
    }

    let seeds = collect_project_seeds(&roots);
    let search_paths: Vec<PathBuf> = roots.clone();

    match resolve_cached(&seeds, &search_paths, libraries, key_inputs, store) {
        Ok(cached) => {
            for name in &cached.selection.required_libraries {
                if let Some(lib) = libraries.iter().find(|l| &l.name == name) {
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
                resolve_framework_library_sources_from_libraries(libraries, &roots),
                false,
            )
        }
    }
}

/// Process-shared `KvStore` for the library-selection cache.
///
/// Opens lazily on first call and caches the handle for the rest of the
/// process. Returns `None` on open failure — callers must skip caching
/// (and route through the uncached resolver) rather than crash.
pub fn library_select_kv_store() -> Option<&'static KvStore> {
    static STORE: OnceLock<Option<KvStore>> = OnceLock::new();
    STORE
        .get_or_init(|| {
            let dir = library_select_cache_dir();
            match KvStore::open(&dir) {
                Ok(store) => {
                    tracing::info!(
                        path = %dir.display(),
                        "library-select cache: opened KvStore"
                    );
                    Some(store)
                }
                Err(err) => {
                    tracing::warn!(
                        path = %dir.display(),
                        error = %err,
                        "library-select cache: failed to open KvStore; \
                         resolution will run uncached"
                    );
                    None
                }
            }
        })
        .as_ref()
}

/// Filesystem location of the library-selection KvStore.
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

/// Collect every source file under each root as a walker seed. Headers are
/// intentionally included so libraries referenced only from a `.h` in the
/// project tree still get picked up.
fn collect_project_seeds(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut seeds = Vec::new();
    for root in roots {
        if !root.exists() {
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
            if is_source_or_header_file(entry.path()) {
                seeds.push(entry.path().to_path_buf());
            }
        }
    }
    seeds
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

fn is_source_or_header_file(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_lowercase();
    matches!(
        ext.as_str(),
        "c" | "cpp" | "cc" | "cxx" | "s" | "ino" | "h" | "hh" | "hpp" | "hxx"
    )
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

    #[test]
    fn cached_resolution_round_trips_through_kvstore() {
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
        let key_inputs = CacheKeyInputs {
            toolchain_triple: "test-arm-none-eabi",
            framework_install_path: &framework_root,
            framework_version: "0.0.0-test",
        };

        let kv = KvStore::open(tmp.path().join("kv")).unwrap();

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
