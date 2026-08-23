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
#[path = "lib_tests.rs"]
mod tests;
