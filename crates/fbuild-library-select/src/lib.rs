//! PlatformIO-LDF-style library resolver.
//!
//! Given a set of seed source files (the sketch and project's `src/` tree), a
//! list of discovered framework libraries, and the project's include
//! roots, `resolve()` returns the set of framework libraries transitively
//! reachable from the seeds plus the compile-set for each selected library.
//!
//! Attribution is by path-prefix: each `#include` is resolved to an absolute
//! path via the walker, then attributed to whichever library's `include_dirs`
//! contain the resolved path as a prefix. No basename-only matching, no
//! filesystem globbing of `.h` files, no mystery overlaps.
//!
//! Convergence is PlatformIO's 2-pass LDF chain:
//! 1. BFS from project seeds. Any library whose include dir contains the
//!    resolved path is marked dependent.
//! 2. Reconciliation: re-walk each dependent library's full source set to
//!    catch anything the header-only pass missed. Libraries newly reached in
//!    pass 2 are also marked dependent.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use fbuild_header_scan::{
    WalkState, active_defines, collect_defined_macro_names, walk_with_state,
    walk_with_state_active_known,
};
use fbuild_packages::library::FrameworkLibrary;
use serde::{Deserialize, Serialize};

pub mod cache;

pub use cache::{CacheKeyInputs, CachedSelection, cache_key, resolve_cached};

/// Stats emitted by [`resolve_with_stats`] for performance assertions and
/// daemon-side observability. `files_read` is the total number of physical
/// `std::fs::read_to_string` invocations across all LDF passes within a
/// single `resolve` call; `passes` is the total pass count (Pass 1 plus
/// every reconciliation iteration that ran, including the final
/// no-change iteration that proved convergence).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResolveStats {
    pub files_read: usize,
    pub passes: usize,
}

/// Resolved library selection plus the transitive include closure.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    /// Canonicalized paths of every file reached by the walker.
    pub included_files: Vec<PathBuf>,
    /// Names of framework libraries whose headers were reached, sorted
    /// lexicographically and deduplicated. The sort is intentional so the
    /// value is a pure function of the *set* of libraries reached, not their
    /// position in the input slice — required for stable cache keys.
    pub required_libraries: Vec<String>,
    /// Source files to compile (sorted, deduped).
    pub source_files: Vec<PathBuf>,
    /// Include dirs to pass to the compiler (sorted, deduped).
    pub include_dirs: Vec<PathBuf>,
    /// Include strings the walker could not resolve (sorted, deduped).
    pub unresolved: Vec<String>,
}

/// Resolve the transitive library selection for a project.
///
/// `seeds` are the source files to walk from (sketch and project `src/`).
/// Local `lib/` and `include/` headers are discovered only through the
/// transitive include graph; they are not independent LDF roots.
/// `project_search_paths` are the project's own include roots — consulted for
/// `<...>` includes before framework libs.
/// `libraries` is the full set of framework libraries discovered under the
/// framework's `libraries/` dir.
pub fn resolve(
    seeds: &[PathBuf],
    project_search_paths: &[PathBuf],
    libraries: &[FrameworkLibrary],
) -> Selection {
    resolve_with_stats(seeds, project_search_paths, libraries).0
}

/// Resolve framework libraries using only includes active for `defines`.
///
/// Build orchestrators must use this entry point so optional framework headers
/// in disabled `#if` branches cannot add object files to the link set.
pub fn resolve_active(
    seeds: &[PathBuf],
    project_search_paths: &[PathBuf],
    libraries: &[FrameworkLibrary],
    defines: &HashMap<String, String>,
) -> Selection {
    resolve_with_stats_active(seeds, project_search_paths, libraries, defines).0
}

/// Same contract as [`resolve`] but also returns [`ResolveStats`] so callers
/// can observe the number of physical file reads and LDF passes performed.
///
/// Internally this is the single implementation; [`resolve`] simply discards
/// the stats. A shared [`WalkState`] is threaded through every pass so that
/// any file scanned by Pass 1 is reused (not re-read) by Pass 2's
/// reconciliation walk. Each pass is wrapped in an `ldf_pass` tracing span;
/// the walker emits its own `ldf_walk` span per BFS invocation.
pub fn resolve_with_stats(
    seeds: &[PathBuf],
    project_search_paths: &[PathBuf],
    libraries: &[FrameworkLibrary],
) -> (Selection, ResolveStats) {
    resolve_with_stats_impl(seeds, project_search_paths, libraries, None)
}

/// Active-branch counterpart to [`resolve_with_stats`].
pub fn resolve_with_stats_active(
    seeds: &[PathBuf],
    project_search_paths: &[PathBuf],
    libraries: &[FrameworkLibrary],
    defines: &HashMap<String, String>,
) -> (Selection, ResolveStats) {
    resolve_with_stats_active_declared(seeds, project_search_paths, libraries, defines, &[])
}

/// [`resolve_with_stats_active`] plus explicitly declared dependencies.
///
/// `declared` holds `lib_deps` entries from `platformio.ini`. Any framework
/// library whose name matches one is selected **regardless of whether the
/// header scan reaches it**, then participates in the reconciliation passes so
/// its own transitive dependencies come along — PlatformIO's semantics for an
/// explicit declaration.
///
/// This is deliberately NOT a deeper LDF. The scan still starts only at
/// project seeds, so the FastLED/fbuild#1094 invariant — an inactive local
/// library header must not select a framework library — is untouched. The
/// only new way in is a user writing the dependency down (FastLED/fbuild#1214).
pub fn resolve_with_stats_active_declared(
    seeds: &[PathBuf],
    project_search_paths: &[PathBuf],
    libraries: &[FrameworkLibrary],
    defines: &HashMap<String, String>,
    declared: &[String],
) -> (Selection, ResolveStats) {
    let effective_defines = seed_defines(seeds, defines);
    resolve_with_stats_impl_declared(
        seeds,
        project_search_paths,
        libraries,
        Some(&effective_defines),
        declared,
    )
}

/// Normalize a `lib_deps` entry to the bare library name used for matching.
///
/// `lib_deps` entries carry owner prefixes and version specs that a framework
/// library's directory name never has:
///
/// ```text
/// SPI                      -> spi
/// Wire@^1.0                -> wire
/// adafruit/Adafruit GFX    -> adafruit gfx
/// ```
///
/// Entries that are URLs or local paths (`https://…`, `file://…`, `./vendor`)
/// name something that has to be *fetched*, not a framework library that is
/// already on disk, so they never match and are left to the installer path.
fn declared_dep_name(entry: &str) -> Option<String> {
    let entry = entry.trim();
    if entry.is_empty() || entry.contains("://") || entry.starts_with('.') || entry.starts_with('/')
    {
        return None;
    }
    // Strip a version spec (`@^1.0`, `@1.2.3`) and then an owner prefix
    // (`owner/Name`), in that order — a version spec can contain `/`.
    let without_version = entry.split('@').next().unwrap_or(entry);
    let bare = without_version
        .rsplit('/')
        .next()
        .unwrap_or(without_version)
        .trim();
    if bare.is_empty() {
        None
    } else {
        Some(bare.to_ascii_lowercase())
    }
}

/// Return declarations that do not match a library bundled with the framework.
///
/// Bundled matches are already selected by [`resolve_with_stats_active_declared`]
/// and must not also be sent to the external registry installer. URL and local
/// path declarations never match a bundled name and remain in the result.
pub fn external_declared_deps(declared: &[String], libraries: &[FrameworkLibrary]) -> Vec<String> {
    let bundled: BTreeSet<String> = libraries
        .iter()
        .map(|library| library.name.to_ascii_lowercase())
        .collect();
    declared
        .iter()
        .filter(|entry| match declared_dep_name(entry) {
            Some(name) => !bundled.contains(&name),
            None => true,
        })
        .cloned()
        .collect()
}

fn seed_defines(
    seeds: &[PathBuf],
    compiler_defines: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut defines = compiler_defines.clone();
    let mut ordered_seeds = seeds.to_vec();
    ordered_seeds.sort();
    for seed in ordered_seeds {
        if let Ok(source) = std::fs::read_to_string(seed) {
            defines = active_defines(&source, &defines);
        }
    }
    defines
}

fn resolve_with_stats_impl(
    seeds: &[PathBuf],
    project_search_paths: &[PathBuf],
    libraries: &[FrameworkLibrary],
    defines: Option<&HashMap<String, String>>,
) -> (Selection, ResolveStats) {
    resolve_with_stats_impl_declared(seeds, project_search_paths, libraries, defines, &[])
}

fn resolve_with_stats_impl_declared(
    seeds: &[PathBuf],
    project_search_paths: &[PathBuf],
    libraries: &[FrameworkLibrary],
    defines: Option<&HashMap<String, String>>,
    declared: &[String],
) -> (Selection, ResolveStats) {
    let mut selected: BTreeSet<usize> = BTreeSet::new();
    let mut all_included: BTreeSet<PathBuf> = BTreeSet::new();
    let mut all_unresolved: BTreeSet<String> = BTreeSet::new();
    let mut state = WalkState::new();
    let mut pass_count: usize = 0;

    let canon_lib_dirs: Vec<Vec<PathBuf>> = libraries
        .iter()
        .map(|lib| lib.include_dirs.iter().map(|d| canon(d)).collect())
        .collect();

    // The walker's search paths include the project's include roots first, then
    // every framework library's include dirs. A reached path is attributed to a
    // library by prefix match, not by which search-path entry matched it — PIO's
    // `search_deps_recursive` semantics. Having all lib include dirs present
    // from the start means pass 1's BFS naturally traverses lib-to-lib edges.
    let mut full_search_paths: Vec<PathBuf> = project_search_paths.to_vec();
    for lib in libraries {
        for d in &lib.include_dirs {
            if !full_search_paths.contains(d) {
                full_search_paths.push(d.clone());
            }
        }
    }

    // Pass 0: explicit `lib_deps` declarations. Selected before any scanning
    // so the reconciliation loop treats them exactly like a scan-selected
    // library and walks their sources for transitive deps
    // (FastLED/fbuild#1214).
    if !declared.is_empty() {
        let wanted: BTreeSet<String> = declared
            .iter()
            .filter_map(|d| declared_dep_name(d))
            .collect();
        for (idx, lib) in libraries.iter().enumerate() {
            if wanted.contains(&lib.name.to_ascii_lowercase()) {
                tracing::info!(library = %lib.name, "ldf: selected by lib_deps declaration");
                selected.insert(idx);
            }
        }
        // A declared entry that matches no framework library is not an error:
        // it is most likely a registry package handled by the installer path.
        // Log it so a typo isn't completely silent.
        let matched: BTreeSet<String> = selected
            .iter()
            .map(|idx| libraries[*idx].name.to_ascii_lowercase())
            .collect();
        for name in wanted.difference(&matched) {
            tracing::debug!(
                dependency = %name,
                "ldf: lib_deps entry matched no framework library"
            );
        }
    }

    // Which macro names the reachable corpus defines anywhere.
    //
    // This is what lets branch evaluation stay honest without hiding real
    // dependencies. `#if defined(FL_IS_SAMD21)` cannot be decided from the
    // compiler command line, because FastLED derives that macro several
    // headers deep and header-defined macros are not threaded through a BFS
    // walk — so guards on names the project *does* define are undecidable and
    // every arm gets scanned. Guards on names nobody defines stay honestly
    // false, which is what keeps a library behind a genuinely dead branch
    // from being selected (FastLED/fbuild#1094, #1371).
    //
    // Only computed when branch evaluation is on; the textual mode already
    // scans every arm.
    let defined_somewhere = if defines.is_some() {
        collect_defined_macro_names(seeds, &full_search_paths)
    } else {
        Default::default()
    };

    // Pass 1: BFS from project seeds.
    {
        let _span = tracing::info_span!("ldf_pass", pass = 1u32).entered();
        pass_count += 1;
        tracing::info!(pass = 1u32, "ldf_pass");
        let res = match defines {
            Some(defines) => walk_with_state_active_known(
                seeds,
                &full_search_paths,
                defines,
                &defined_somewhere,
                &mut state,
            ),
            None => walk_with_state(seeds, &full_search_paths, &mut state),
        };
        for p in &res.reached {
            all_included.insert(p.clone());
        }
        for u in &res.unresolved {
            all_unresolved.insert(u.clone());
        }
        for (idx, dirs) in canon_lib_dirs.iter().enumerate() {
            if res.reached.iter().any(|p| path_in_any(p, dirs)) {
                selected.insert(idx);
            }
        }
    }

    // Pass 2+: reconciliation. Re-walk with each selected library's full
    // source set as seeds, in case a lib-to-lib dep is only visible through a
    // `.cpp` (not a header). Keeps iterating until the selection stabilizes,
    // which for realistic Arduino-library graphs is 1–2 rounds. With the
    // shared `WalkState`, `res.reached` is the *delta* of newly-discovered
    // files for this pass -- the prefix-match check still works correctly
    // because a library can only become newly-selected via a path reached for
    // the first time in this pass.
    loop {
        pass_count += 1;
        let _span = tracing::info_span!("ldf_pass", pass = pass_count as u32).entered();
        tracing::info!(pass = pass_count as u32, "ldf_pass");
        let mut recon_seeds: Vec<PathBuf> = seeds.to_vec();
        for idx in &selected {
            for src in &libraries[*idx].source_files {
                recon_seeds.push(src.clone());
            }
        }
        let res = match defines {
            Some(defines) => walk_with_state_active_known(
                &recon_seeds,
                &full_search_paths,
                defines,
                &defined_somewhere,
                &mut state,
            ),
            None => walk_with_state(&recon_seeds, &full_search_paths, &mut state),
        };
        for p in &res.reached {
            all_included.insert(p.clone());
        }
        for u in &res.unresolved {
            all_unresolved.insert(u.clone());
        }
        let before = selected.len();
        for (idx, dirs) in canon_lib_dirs.iter().enumerate() {
            if selected.contains(&idx) {
                continue;
            }
            if res.reached.iter().any(|p| path_in_any(p, dirs)) {
                selected.insert(idx);
            }
        }
        if selected.len() == before {
            break;
        }
    }

    let mut required_libraries: Vec<String> = Vec::new();
    let mut source_files: BTreeSet<PathBuf> = BTreeSet::new();
    let mut include_dirs: BTreeMap<PathBuf, ()> = BTreeMap::new();
    for idx in &selected {
        let lib = &libraries[*idx];
        required_libraries.push(lib.name.clone());
        for s in &lib.source_files {
            source_files.insert(s.clone());
        }
        for d in &lib.include_dirs {
            include_dirs.insert(d.clone(), ());
        }
    }
    // Sort by name so the output is a deterministic function of the input
    // *set* of libraries rather than their input order — required for stable
    // cache keys in #205 Phase 4.
    required_libraries.sort();
    required_libraries.dedup();

    let selection = Selection {
        included_files: all_included.into_iter().collect(),
        required_libraries,
        source_files: source_files.into_iter().collect(),
        include_dirs: include_dirs.into_keys().collect(),
        unresolved: all_unresolved.into_iter().collect(),
    };
    let stats = ResolveStats {
        files_read: state.files_read(),
        passes: pass_count,
    };
    (selection, stats)
}

fn canon(p: &Path) -> PathBuf {
    // FastLED/fbuild#844 sync-context allowlist: `resolve_with_stats`
    // is sync (called from the daemon's `BuildOrchestrator` chain and
    // from the diagnostic `fbuild lib-select` CLI). Making it async to
    // adopt `fbuild_core::path::canonicalize_existing` would cascade
    // through every caller. File is allowlisted in
    // `dylints/ban_std_fs_canonicalize/src/allowlist.txt`.
    match std::fs::canonicalize(p) {
        Ok(c) => c,
        Err(err) => {
            // The walker canonicalizes every reached path, so an
            // un-canonicalized library include dir won't `starts_with`-match
            // anything on macOS (`/var` vs `/private/var`) or Windows (`\\?\`
            // vs plain). Warn loudly so a missing/relocated framework install
            // shows up in logs instead of as a silent "library not selected"
            // false negative at link time.
            tracing::warn!(
                path = %p.display(),
                error = %err,
                "fbuild-library-select: failed to canonicalize path; \
                 prefix-attribution may miss this directory"
            );
            p.to_path_buf()
        }
    }
}

fn path_in_any(path: &Path, dirs: &[PathBuf]) -> bool {
    dirs.iter().any(|d| path.starts_with(d))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

    fn lib(tmp: &Path, name: &str) -> FrameworkLibrary {
        let dir = tmp.join("libraries").join(name);
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        FrameworkLibrary {
            name: name.to_string(),
            dir: dir.clone(),
            include_dirs: vec![src.clone()],
            source_files: Vec::new(),
        }
    }

    fn tempdir() -> TempDir {
        TempDir::new_in(fbuild_paths::temp_subdir("fbuild-library-select-tests")).unwrap()
    }

    #[test]
    fn r01_direct_include_selects_library() {
        let tmp = tempdir();
        let project_src = tmp.path().join("project").join("src");
        write(&project_src.join("main.cpp"), "#include <SPI.h>\n");
        let mut spi = lib(tmp.path(), "SPI");
        write(&spi.include_dirs[0].join("SPI.h"), "");
        let spi_cpp = spi.include_dirs[0].join("SPI.cpp");
        write(&spi_cpp, "");
        spi.source_files.push(spi_cpp.clone());

        let seeds = vec![project_src.join("main.cpp")];
        let sel = resolve(&seeds, &[project_src], &[spi]);
        assert_eq!(sel.required_libraries, vec!["SPI".to_string()]);
        assert!(sel.source_files.contains(&canon(&spi_cpp)) || sel.source_files.contains(&spi_cpp));
    }

    /// FastLED/fbuild#1371, end to end: a macro derived inside a header must
    /// not hide the include it guards.
    ///
    /// This is the SAMD shape verbatim. The compiler command line carries
    /// `__SAMD21G18A__`; `FL_IS_SAMD21` is derived from it several headers
    /// deep, and header-defined macros are not threaded through a BFS walk.
    /// Evaluating `#if defined(FL_IS_SAMD21)` as *false* made `<SPI.h>`
    /// invisible to selection even though the include is genuinely compiled —
    /// the build then failed on a missing header that nothing had requested.
    #[test]
    fn include_guarded_by_a_header_derived_macro_still_selects_its_library() {
        let tmp = tempdir();
        let project_src = tmp.path().join("project").join("src");
        write(&project_src.join("main.cpp"), "#include <FastLED.h>\n");
        write(
            &project_src.join("FastLED.h"),
            "#include <is_platform.h>\n#include <fastspi_arm_sam.h>\n",
        );
        // The derivation the walk cannot see: command-line macro in, FastLED
        // macro out.
        write(
            &project_src.join("is_platform.h"),
            "#if defined(__SAMD21G18A__)\n#define FL_IS_SAMD21 1\n#endif\n",
        );
        write(
            &project_src.join("fastspi_arm_sam.h"),
            "#if defined(FL_IS_SAMD21) || defined(FL_IS_SAMD51)\n#include <SPI.h>\n#endif\n",
        );

        let mut spi = lib(tmp.path(), "SPI");
        write(&spi.include_dirs[0].join("SPI.h"), "");
        let spi_cpp = spi.include_dirs[0].join("SPI.cpp");
        write(&spi_cpp, "");
        spi.source_files.push(spi_cpp);

        let mut defines = HashMap::new();
        defines.insert("__SAMD21G18A__".to_string(), "1".to_string());

        let seeds = vec![project_src.join("main.cpp")];
        let selection = resolve_active(&seeds, &[project_src], &[spi], &defines);
        assert_eq!(
            selection.required_libraries,
            vec!["SPI".to_string()],
            "an include behind a header-derived guard must still select its library"
        );
    }

    /// The `#if 0` LDF hint idiom, end to end.
    ///
    /// `platforms/ldf_headers.h` in FastLED declares dependencies inside
    /// `#if 0` blocks precisely because PlatformIO's `chain` LDF scans
    /// includes without evaluating conditionals. The block never compiles, so
    /// an include there exists only to be seen.
    #[test]
    fn if_zero_hint_headers_declare_a_dependency() {
        let tmp = tempdir();
        let project_src = tmp.path().join("project").join("src");
        write(&project_src.join("main.cpp"), "#include <FastLED.h>\n");
        write(&project_src.join("FastLED.h"), "#include <ldf_headers.h>\n");
        write(
            &project_src.join("ldf_headers.h"),
            "#if 0\n#include <SPI.h>\n#endif\n",
        );

        let mut spi = lib(tmp.path(), "SPI");
        write(&spi.include_dirs[0].join("SPI.h"), "");
        let spi_cpp = spi.include_dirs[0].join("SPI.cpp");
        write(&spi_cpp, "");
        spi.source_files.push(spi_cpp);

        let seeds = vec![project_src.join("main.cpp")];
        let selection = resolve_active(&seeds, &[project_src], &[spi], &HashMap::new());
        assert_eq!(
            selection.required_libraries,
            vec!["SPI".to_string()],
            "an #if 0 hint must declare the dependency"
        );
    }

    /// A guard on a macro *nothing* defines stays honestly false.
    ///
    /// The counterweight to the two tests above, and the reason this is not
    /// simply a textual scan: without it, every unresolved guard would select
    /// its library and #1094's over-selection would come straight back.
    #[test]
    fn guard_on_a_macro_no_file_defines_does_not_select() {
        let tmp = tempdir();
        let project_src = tmp.path().join("project").join("src");
        write(&project_src.join("main.cpp"), "#include <FastLED.h>\n");
        write(
            &project_src.join("FastLED.h"),
            "#if defined(NOBODY_DEFINES_THIS)\n#include <Audio.h>\n#endif\n#include <SPI.h>\n",
        );

        let mut audio = lib(tmp.path(), "Audio");
        write(&audio.include_dirs[0].join("Audio.h"), "");
        let audio_cpp = audio.include_dirs[0].join("Audio.cpp");
        write(&audio_cpp, "");
        audio.source_files.push(audio_cpp);

        let mut spi = lib(tmp.path(), "SPI");
        write(&spi.include_dirs[0].join("SPI.h"), "");
        let spi_cpp = spi.include_dirs[0].join("SPI.cpp");
        write(&spi_cpp, "");
        spi.source_files.push(spi_cpp);

        let seeds = vec![project_src.join("main.cpp")];
        let selection = resolve_active(&seeds, &[project_src], &[audio, spi], &HashMap::new());
        assert_eq!(
            selection.required_libraries,
            vec!["SPI".to_string()],
            "a guard nothing can satisfy must not pull in a library"
        );
    }

    #[test]
    fn active_resolution_skips_library_in_disabled_branch() {
        let tmp = tempdir();
        let project_src = tmp.path().join("project").join("src");
        write(
            &project_src.join("main.cpp"),
            "#define USE_SPI 1\n#include <FastLED.h>\n",
        );
        write(
            &project_src.join("FastLED.h"),
            "#if defined(USE_AUDIO)\n#include <Audio.h>\n#elif USE_SPI\n#include <SPI.h>\n#endif\n",
        );

        let mut audio = lib(tmp.path(), "Audio");
        write(&audio.include_dirs[0].join("Audio.h"), "");
        let audio_cpp = audio.include_dirs[0].join("Audio.cpp");
        write(&audio_cpp, "");
        audio.source_files.push(audio_cpp);

        let mut spi = lib(tmp.path(), "SPI");
        write(&spi.include_dirs[0].join("SPI.h"), "");
        let spi_cpp = spi.include_dirs[0].join("SPI.cpp");
        write(&spi_cpp, "");
        spi.source_files.push(spi_cpp);

        let seeds = vec![project_src.join("main.cpp")];
        let selection = resolve_active(&seeds, &[project_src], &[audio, spi], &HashMap::new());
        assert_eq!(selection.required_libraries, vec!["SPI".to_string()]);
    }

    #[test]
    fn r02_transitive_library_selection() {
        let tmp = tempdir();
        let project_src = tmp.path().join("project").join("src");
        write(&project_src.join("main.cpp"), "#include <SPI.h>\n");

        let mut spi = lib(tmp.path(), "SPI");
        write(&spi.include_dirs[0].join("SPI.h"), "#include <Wire.h>\n");
        let spi_cpp = spi.include_dirs[0].join("SPI.cpp");
        write(&spi_cpp, "");
        spi.source_files.push(spi_cpp);

        let mut wire = lib(tmp.path(), "Wire");
        write(&wire.include_dirs[0].join("Wire.h"), "");
        let wire_cpp = wire.include_dirs[0].join("Wire.cpp");
        write(&wire_cpp, "");
        wire.source_files.push(wire_cpp);

        let seeds = vec![project_src.join("main.cpp")];
        let sel = resolve(&seeds, &[project_src], &[spi, wire]);
        assert_eq!(
            sel.required_libraries,
            vec!["SPI".to_string(), "Wire".to_string()]
        );
    }

    #[test]
    fn r04_pass2_reconciliation_catches_cpp_only_dependency() {
        // The whole reason the LDF resolver is 2-pass instead of single-pass
        // BFS: a lib's `.cpp` may pull in a second lib that the first lib's
        // `.h` does NOT mention. Pass 1 (BFS from project seeds + reached
        // headers) cannot see that edge; pass 2 re-seeds with each selected
        // lib's full source set and catches it.
        //
        // Setup: project includes <SPI.h>. SPI.h is silent. SPI.cpp includes
        // <Wire.h>. Wire is only reachable through SPI.cpp.
        //
        // Expected: pass 1 selects {SPI}; pass 2 (with SPI.cpp as a seed)
        // selects {SPI, Wire}. A regression that drops the second pass would
        // produce {SPI} only and silently miss Wire at link time.
        let tmp = tempdir();
        let project_src = tmp.path().join("project").join("src");
        write(&project_src.join("main.cpp"), "#include <SPI.h>\n");

        let mut spi = lib(tmp.path(), "SPI");
        write(
            &spi.include_dirs[0].join("SPI.h"),
            "// no transitive includes\n",
        );
        let spi_cpp = spi.include_dirs[0].join("SPI.cpp");
        write(&spi_cpp, "#include <Wire.h>\n");
        spi.source_files.push(spi_cpp);

        let mut wire = lib(tmp.path(), "Wire");
        write(&wire.include_dirs[0].join("Wire.h"), "");
        let wire_cpp = wire.include_dirs[0].join("Wire.cpp");
        write(&wire_cpp, "");
        wire.source_files.push(wire_cpp);

        let seeds = vec![project_src.join("main.cpp")];
        let sel = resolve(&seeds, &[project_src], &[spi, wire]);
        assert_eq!(
            sel.required_libraries,
            vec!["SPI".to_string(), "Wire".to_string()],
            "pass 2 reconciliation must catch Wire reached only via SPI.cpp"
        );
    }

    #[test]
    fn r03_no_includes_selects_nothing() {
        let tmp = tempdir();
        let project_src = tmp.path().join("project").join("src");
        write(&project_src.join("main.cpp"), "int main() { return 0; }\n");
        let spi = lib(tmp.path(), "SPI");
        write(&spi.include_dirs[0].join("SPI.h"), "");

        let seeds = vec![project_src.join("main.cpp")];
        let sel = resolve(&seeds, &[project_src], &[spi]);
        assert!(sel.required_libraries.is_empty());
    }

    #[test]
    fn r13_unrelated_library_not_selected() {
        let tmp = tempdir();
        let project_src = tmp.path().join("project").join("src");
        write(&project_src.join("main.cpp"), "#include <SPI.h>\n");

        let mut spi = lib(tmp.path(), "SPI");
        write(&spi.include_dirs[0].join("SPI.h"), "");
        let spi_cpp = spi.include_dirs[0].join("SPI.cpp");
        write(&spi_cpp, "");
        spi.source_files.push(spi_cpp);

        let mut fnet = lib(tmp.path(), "FNET");
        write(&fnet.include_dirs[0].join("fnet.h"), "");
        let fnet_cpp = fnet.include_dirs[0].join("fnet.cpp");
        write(&fnet_cpp, "");
        fnet.source_files.push(fnet_cpp);

        let seeds = vec![project_src.join("main.cpp")];
        let sel = resolve(&seeds, &[project_src], &[spi, fnet]);
        assert_eq!(sel.required_libraries, vec!["SPI".to_string()]);
    }

    #[test]
    fn path_prefix_attribution_distinguishes_same_basename() {
        let tmp = tempdir();
        let project_src = tmp.path().join("project").join("src");
        write(&project_src.join("main.cpp"), "#include \"foo/config.h\"\n");

        let mut foo = lib(tmp.path(), "Foo");
        write(&foo.include_dirs[0].join("foo").join("config.h"), "");
        let foo_cpp = foo.include_dirs[0].join("Foo.cpp");
        write(&foo_cpp, "");
        foo.source_files.push(foo_cpp);

        let mut bar = lib(tmp.path(), "Bar");
        // Bar also has a config.h but at its own path — must NOT be selected
        // when the project only includes "foo/config.h".
        write(&bar.include_dirs[0].join("bar").join("config.h"), "");
        let bar_cpp = bar.include_dirs[0].join("Bar.cpp");
        write(&bar_cpp, "");
        bar.source_files.push(bar_cpp);

        let seeds = vec![project_src.join("main.cpp")];
        let sel = resolve(
            &seeds,
            &[
                project_src,
                foo.include_dirs[0].clone(),
                bar.include_dirs[0].clone(),
            ],
            &[foo, bar],
        );
        assert_eq!(sel.required_libraries, vec!["Foo".to_string()]);
    }

    #[test]
    fn empty_libraries_yields_empty_selection() {
        // Adversary: no libraries at all. resolve must terminate cleanly with
        // no required_libraries, no panics, and any reached files limited to
        // what the walker found from seeds alone.
        let tmp = tempdir();
        let project_src = tmp.path().join("project").join("src");
        write(&project_src.join("main.cpp"), "int main() { return 0; }\n");
        let seeds = vec![project_src.join("main.cpp")];
        let sel = resolve(&seeds, &[project_src], &[]);
        assert!(sel.required_libraries.is_empty());
        assert!(sel.source_files.is_empty());
    }

    #[test]
    fn missing_library_include_dir_does_not_panic() {
        // Adversary: a FrameworkLibrary whose include_dirs point at a path
        // that doesn't exist on disk (broken framework install, lib not yet
        // downloaded). canon() falls back and emits a tracing::warn; the
        // resolver must not panic and must return a sensible empty
        // selection.
        let tmp = tempdir();
        let project_src = tmp.path().join("project").join("src");
        write(&project_src.join("main.cpp"), "int main() { return 0; }\n");
        let phantom = FrameworkLibrary {
            name: "Phantom".to_string(),
            dir: tmp.path().join("nonexistent").join("Phantom"),
            include_dirs: vec![tmp.path().join("nonexistent").join("Phantom").join("src")],
            source_files: Vec::new(),
        };
        let seeds = vec![project_src.join("main.cpp")];
        let sel = resolve(&seeds, &[project_src], &[phantom]);
        assert!(sel.required_libraries.is_empty());
    }

    #[test]
    fn many_libraries_in_random_order_returns_sorted() {
        // Adversary: 6 libs in deliberately scrambled input order. The
        // output must be sorted lexicographically, independent of input
        // order — required for stable cache keys (#205 Phase 4).
        let tmp = tempdir();
        let project_src = tmp.path().join("project").join("src");
        write(
            &project_src.join("main.cpp"),
            "#include <Z.h>\n#include <A.h>\n#include <M.h>\n\
             #include <B.h>\n#include <Y.h>\n#include <K.h>\n",
        );

        let mut libs = Vec::new();
        for name in ["Z", "A", "M", "B", "Y", "K"] {
            let mut l = lib(tmp.path(), name);
            write(&l.include_dirs[0].join(format!("{name}.h")), "");
            let cpp = l.include_dirs[0].join(format!("{name}.cpp"));
            write(&cpp, "");
            l.source_files.push(cpp);
            libs.push(l);
        }

        let seeds = vec![project_src.join("main.cpp")];
        let sel = resolve(&seeds, &[project_src], &libs);
        assert_eq!(
            sel.required_libraries,
            ["A", "B", "K", "M", "Y", "Z"]
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn required_libraries_returned_sorted_by_name_not_input_order() {
        // Regression guard: pass the libraries in REVERSE name order (Wire
        // before SPI) and confirm the output is sorted lexicographically.
        // The doc on `Selection::required_libraries` and the cache-key story
        // in #205 Phase 4 both depend on this being a pure function of the
        // selected *set* of libraries, not their input position.
        let tmp = tempdir();
        let project_src = tmp.path().join("project").join("src");
        write(
            &project_src.join("main.cpp"),
            "#include <SPI.h>\n#include <Wire.h>\n",
        );

        let mut spi = lib(tmp.path(), "SPI");
        write(&spi.include_dirs[0].join("SPI.h"), "");
        let spi_cpp = spi.include_dirs[0].join("SPI.cpp");
        write(&spi_cpp, "");
        spi.source_files.push(spi_cpp);

        let mut wire = lib(tmp.path(), "Wire");
        write(&wire.include_dirs[0].join("Wire.h"), "");
        let wire_cpp = wire.include_dirs[0].join("Wire.cpp");
        write(&wire_cpp, "");
        wire.source_files.push(wire_cpp);

        let seeds = vec![project_src.join("main.cpp")];
        // Wire is passed BEFORE SPI in the input slice.
        let sel = resolve(&seeds, &[project_src], &[wire, spi]);
        assert_eq!(
            sel.required_libraries,
            vec!["SPI".to_string(), "Wire".to_string()]
        );
    }

    // ---- lib_deps declarations (FastLED/fbuild#1214) ------------------------

    /// Build the exact shape from the issue: a sketch that reaches SPI only
    /// through a *library* header, which the shallow scan deliberately does
    /// not follow. Returns (seeds, search paths, libs, SPI's .cpp).
    fn unreachable_spi_fixture(
        tmp: &Path,
    ) -> (Vec<PathBuf>, Vec<PathBuf>, Vec<FrameworkLibrary>, PathBuf) {
        let project_src = tmp.join("project").join("src");
        write(&project_src.join("main.cpp"), "// no includes at all\n");

        let mut spi = lib(tmp, "SPI");
        write(&spi.include_dirs[0].join("SPI.h"), "");
        let spi_cpp = spi.include_dirs[0].join("SPI.cpp");
        write(&spi_cpp, "");
        spi.source_files.push(spi_cpp.clone());

        (
            vec![project_src.join("main.cpp")],
            vec![project_src],
            vec![spi],
            spi_cpp,
        )
    }

    /// Baseline: without a declaration the library is NOT selected. This is
    /// the shallow-LDF default (#1094) and must stay that way — the fix adds
    /// an opt-in, it does not deepen the scan.
    #[test]
    fn unreached_library_is_not_selected_without_a_declaration() {
        let tmp = tempdir();
        let (seeds, paths, libs, _) = unreachable_spi_fixture(tmp.path());

        let sel = resolve_with_stats_active_declared(&seeds, &paths, &libs, &HashMap::new(), &[]).0;

        assert!(sel.required_libraries.is_empty(), "{sel:?}");
    }

    #[test]
    fn lib_deps_declaration_selects_an_unreached_library() {
        let tmp = tempdir();
        let (seeds, paths, libs, spi_cpp) = unreachable_spi_fixture(tmp.path());

        let sel = resolve_with_stats_active_declared(
            &seeds,
            &paths,
            &libs,
            &HashMap::new(),
            &["SPI".to_string()],
        )
        .0;

        assert_eq!(sel.required_libraries, vec!["SPI".to_string()]);
        assert!(
            sel.source_files.contains(&canon(&spi_cpp)) || sel.source_files.contains(&spi_cpp),
            "declared library's sources must reach the link line: {sel:?}"
        );
    }

    /// PlatformIO matches `lib_deps` entries case-insensitively and tolerates
    /// owner prefixes and version specs. A declaration that doesn't match
    /// because of a `@^1.0` suffix would look like the feature is broken.
    #[test]
    fn lib_deps_matching_ignores_case_owner_and_version() {
        for entry in ["spi", "SPI@^1.0", "arduino/SPI", "arduino/SPI@1.2.3"] {
            let tmp = tempdir();
            let (seeds, paths, libs, _) = unreachable_spi_fixture(tmp.path());

            let sel = resolve_with_stats_active_declared(
                &seeds,
                &paths,
                &libs,
                &HashMap::new(),
                &[entry.to_string()],
            )
            .0;

            assert_eq!(
                sel.required_libraries,
                vec!["SPI".to_string()],
                "entry {entry:?} should match the SPI framework library"
            );
        }
    }

    /// URLs and local paths name something to be *fetched*; they must not be
    /// mangled into a bare name that accidentally matches a framework library.
    #[test]
    fn lib_deps_urls_and_paths_never_match_a_framework_library() {
        for entry in [
            "https://github.com/example/SPI.git",
            "file:///opt/SPI",
            "./vendor/SPI",
        ] {
            let tmp = tempdir();
            let (seeds, paths, libs, _) = unreachable_spi_fixture(tmp.path());

            let sel = resolve_with_stats_active_declared(
                &seeds,
                &paths,
                &libs,
                &HashMap::new(),
                &[entry.to_string()],
            )
            .0;

            assert!(
                sel.required_libraries.is_empty(),
                "entry {entry:?} must be left to the installer path, got {sel:?}"
            );
        }
    }

    /// An explicit declaration gets its own dependency chain resolved, exactly
    /// as PlatformIO does — the declared library participates in the
    /// reconciliation passes rather than being bolted on at the end.
    #[test]
    fn declared_library_pulls_in_its_own_transitive_dependency() {
        let tmp = tempdir();
        let project_src = tmp.path().join("project").join("src");
        write(&project_src.join("main.cpp"), "// no includes\n");

        let mut wire = lib(tmp.path(), "Wire");
        write(&wire.include_dirs[0].join("Wire.h"), "");
        let wire_cpp = wire.include_dirs[0].join("Wire.cpp");
        write(&wire_cpp, "");
        wire.source_files.push(wire_cpp);

        // SPI.cpp — not its header — is what reaches Wire, so only the
        // reconciliation pass can find it.
        let mut spi = lib(tmp.path(), "SPI");
        write(&spi.include_dirs[0].join("SPI.h"), "");
        let spi_cpp = spi.include_dirs[0].join("SPI.cpp");
        write(&spi_cpp, "#include <Wire.h>\n");
        spi.source_files.push(spi_cpp);

        let seeds = vec![project_src.join("main.cpp")];
        let sel = resolve_with_stats_active_declared(
            &seeds,
            &[project_src],
            &[spi, wire],
            &HashMap::new(),
            &["SPI".to_string()],
        )
        .0;

        assert_eq!(
            sel.required_libraries,
            vec!["SPI".to_string(), "Wire".to_string()],
            "declaring SPI must also bring in the Wire it depends on"
        );
    }

    #[test]
    fn bundled_framework_lib_deps_are_not_external() {
        let tmp = tempdir();
        let libraries = vec![lib(tmp.path(), "BTstackLib"), lib(tmp.path(), "HTTPUpdate")];
        let declared = vec![
            "btSTACKlib".to_string(),
            "vendor/HTTPUpdate@^1.3".to_string(),
            "ExternalRegistryLib@^2.0".to_string(),
            "https://example.com/vendor/local-lib.git".to_string(),
            "https://example.com/vendor/HTTPUpdate".to_string(),
            "file://vendor/BTstackLib".to_string(),
        ];

        assert_eq!(
            external_declared_deps(&declared, &libraries),
            vec![
                "ExternalRegistryLib@^2.0".to_string(),
                "https://example.com/vendor/local-lib.git".to_string(),
                "https://example.com/vendor/HTTPUpdate".to_string(),
                "file://vendor/BTstackLib".to_string(),
            ]
        );
    }

    #[test]
    fn declared_dep_name_normalization() {
        assert_eq!(declared_dep_name("SPI").as_deref(), Some("spi"));
        assert_eq!(declared_dep_name("  Wire@^1.0  ").as_deref(), Some("wire"));
        assert_eq!(
            declared_dep_name("adafruit/Adafruit GFX").as_deref(),
            Some("adafruit gfx")
        );
        assert_eq!(declared_dep_name(""), None);
        assert_eq!(declared_dep_name("https://example.com/x.git"), None);
        assert_eq!(declared_dep_name("./local"), None);
    }
}
