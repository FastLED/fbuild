//! Transitive include-graph walker.
//!
//! Given a set of seed source files and an ordered list of search paths, walks
//! every reachable `#include` and returns the set of resolved files (sorted)
//! plus the set of include strings that could not be resolved. The walker is
//! BFS over a visited set so cycles, diamonds, and arbitrary depth all
//! terminate correctly.
//!
//! Two public entry points:
//! * [`walk`] -- one-shot convenience wrapper that allocates a fresh
//!   [`WalkState`] internally. `WalkResult::reached` is the full set of files
//!   reached from `seeds`.
//! * [`walk_with_state`] -- accepts a caller-owned [`WalkState`] so multiple
//!   walks can share a scan cache and a `visited` set across calls (used by
//!   `fbuild-library-select` to avoid re-reading files between LDF passes).
//!   `WalkResult::reached` is the *delta* of canonical paths newly discovered
//!   in this call; the union of deltas across calls equals the full set.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::scanner::{scan, IncludeKind, IncludeRef};

/// Result of a walk. `reached` and `unresolved` are sorted for deterministic
/// cache keys.
///
/// For [`walk`] (fresh-state wrapper) `reached` is the full set of files
/// transitively reached from the seeds. For [`walk_with_state`] the same
/// fields contain only the *delta* added in this call -- files already
/// present in the shared `WalkState::visited` set are not re-emitted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WalkResult {
    pub reached: Vec<PathBuf>,
    pub unresolved: Vec<String>,
}

/// State that can be shared across multiple [`walk_with_state`] calls so the
/// include-scan results are memoized and each on-disk file is read at most
/// once for the lifetime of the state.
///
/// Used by `fbuild-library-select::resolve_with_stats` to share scan results
/// across LDF passes -- pass 1 reads every file once, pass 2 re-seeds with
/// library `.cpp` files but reuses the cached scans for everything already
/// reached.
#[derive(Debug, Default)]
pub struct WalkState {
    /// Canonical paths the walker has already enqueued/visited.
    visited: HashSet<PathBuf>,
    /// Canonical path -> parsed include list. Populated lazily on first read.
    /// Missing entries mean either "not yet read" or "read failed" -- they are
    /// indistinguishable here, matching the existing `let Ok(...) else
    /// { continue }` semantics of the original walker.
    scan_cache: HashMap<PathBuf, Vec<IncludeRef>>,
    /// Number of successful `std::fs::read_to_string` invocations across the
    /// lifetime of this state. Each unique file is counted exactly once
    /// because subsequent walks hit `scan_cache` instead.
    files_read: usize,
}

impl WalkState {
    /// Create an empty state. No files have been scanned, nothing is visited.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of files physically read from disk so far. Used by
    /// `resolve_with_stats` to assert the no-re-read contract in tests.
    pub fn files_read(&self) -> usize {
        self.files_read
    }
}

/// Walk the include graph starting from `seeds` over `search_paths`.
///
/// `search_paths` is consulted in order for `<...>` includes and as a
/// secondary lookup for `"..."` includes (after the same-directory check).
/// A file is added to `reached` exactly once. Files outside `search_paths`
/// are still reached if they are seeds or `"..."`-resolved relative to a
/// seed/visited file.
///
/// Allocates a fresh [`WalkState`] internally, so `WalkResult::reached`
/// contains every file transitively reached from `seeds`.
pub fn walk(seeds: &[PathBuf], search_paths: &[PathBuf]) -> WalkResult {
    let mut state = WalkState::new();
    walk_with_state(seeds, search_paths, &mut state)
}

/// Walk the include graph using a caller-owned [`WalkState`] so the scan cache
/// and visited set persist across calls.
///
/// `WalkResult::reached` contains only the *delta* of canonical paths newly
/// reached in this call. Files already in `state.visited` from a previous
/// call are not re-emitted (and not re-read).
///
/// The BFS proceeds in waves: each wave reads all not-yet-cached files in
/// parallel via rayon, then resolves every `#include` in every cached scan
/// result to enqueue the next wave.
#[tracing::instrument(
    name = "ldf_walk",
    skip_all,
    fields(seeds = seeds.len(), search_paths = search_paths.len())
)]
pub fn walk_with_state(
    seeds: &[PathBuf],
    search_paths: &[PathBuf],
    state: &mut WalkState,
) -> WalkResult {
    tracing::debug!(
        seeds = seeds.len(),
        search_paths = search_paths.len(),
        "ldf_walk"
    );
    let mut reached: BTreeSet<PathBuf> = BTreeSet::new();
    let mut unresolved: BTreeSet<String> = BTreeSet::new();
    let mut frontier: VecDeque<PathBuf> = VecDeque::new();

    for seed in seeds {
        let canon = canon(seed);
        if state.visited.insert(canon.clone()) {
            frontier.push_back(canon.clone());
            reached.insert(canon);
        }
    }

    while !frontier.is_empty() {
        // Read all not-yet-cached files in the current wave in parallel.
        let to_read: Vec<PathBuf> = frontier
            .iter()
            .filter(|p| !state.scan_cache.contains_key(*p))
            .cloned()
            .collect();

        if !to_read.is_empty() {
            let scanned: Vec<(PathBuf, Vec<IncludeRef>)> = to_read
                .par_iter()
                .filter_map(|p| {
                    let text = std::fs::read_to_string(p).ok()?;
                    Some((p.clone(), scan(&text)))
                })
                .collect();

            for (path, includes) in scanned {
                state.scan_cache.insert(path, includes);
                state.files_read += 1;
            }
        }

        // Resolve includes for every file in the frontier and build the next
        // wave from any newly discovered canonical paths.
        let current: Vec<PathBuf> = frontier.drain(..).collect();
        for file in &current {
            let Some(includes) = state.scan_cache.get(file).cloned() else {
                // Read failed (file is a directory, permission denied, etc.).
                // Match the existing behavior: silently skip.
                continue;
            };
            for inc in &includes {
                match resolve_include(inc, file, search_paths) {
                    Some(resolved) => {
                        let canon = canon(&resolved);
                        if state.visited.insert(canon.clone()) {
                            reached.insert(canon.clone());
                            frontier.push_back(canon);
                        }
                    }
                    None => {
                        unresolved.insert(inc.path.clone());
                    }
                }
            }
        }
    }

    WalkResult {
        reached: reached.into_iter().collect(),
        unresolved: unresolved.into_iter().collect(),
    }
}

fn resolve_include(inc: &IncludeRef, from: &Path, search_paths: &[PathBuf]) -> Option<PathBuf> {
    if inc.kind == IncludeKind::Quoted {
        if let Some(parent) = from.parent() {
            let candidate = parent.join(&inc.path);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    for sp in search_paths {
        let candidate = sp.join(&inc.path);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
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

    #[test]
    fn w01_quoted_resolves_same_dir_first() {
        let tmp = TempDir::new().unwrap();
        let main = tmp.path().join("main.cpp");
        let local = tmp.path().join("foo.h");
        let other = tmp.path().join("other").join("foo.h");
        write(&main, "#include \"foo.h\"\n");
        write(&local, "// local\n");
        write(&other, "// other\n");

        let res = walk(std::slice::from_ref(&main), &[tmp.path().join("other")]);
        assert!(
            res.reached
                .iter()
                .any(|p| p.ends_with("foo.h") && !p.starts_with(tmp.path().join("other"))),
            "expected local foo.h, got: {:?}",
            res.reached
        );
    }

    #[test]
    fn w02_angled_skips_same_dir() {
        let tmp = TempDir::new().unwrap();
        let main = tmp.path().join("main.cpp");
        let local = tmp.path().join("foo.h");
        let other_dir = tmp.path().join("other");
        let other = other_dir.join("foo.h");
        write(&main, "#include <foo.h>\n");
        write(&local, "// local\n");
        write(&other, "// other\n");

        let res = walk(
            std::slice::from_ref(&main),
            std::slice::from_ref(&other_dir),
        );
        let canon_other = std::fs::canonicalize(&other).unwrap();
        assert!(
            res.reached.contains(&canon_other),
            "expected angled to resolve via search path, got: {:?}",
            res.reached
        );
    }

    #[test]
    fn w03_search_path_precedence_first_hit_wins() {
        let tmp = TempDir::new().unwrap();
        let main = tmp.path().join("main.cpp");
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        write(&a.join("dup.h"), "// a\n");
        write(&b.join("dup.h"), "// b\n");
        write(&main, "#include <dup.h>\n");

        let res = walk(std::slice::from_ref(&main), &[a.clone(), b.clone()]);
        let canon_a = std::fs::canonicalize(a.join("dup.h")).unwrap();
        assert!(res.reached.contains(&canon_a));
    }

    #[test]
    fn w04_missing_header_goes_to_unresolved() {
        let tmp = TempDir::new().unwrap();
        let main = tmp.path().join("main.cpp");
        write(&main, "#include <does_not_exist.h>\n");
        let res = walk(std::slice::from_ref(&main), &[]);
        assert!(res.unresolved.iter().any(|s| s == "does_not_exist.h"));
    }

    #[test]
    fn w10_cycle_terminates() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("a.h");
        let b = tmp.path().join("b.h");
        write(&a, "#include \"b.h\"\n");
        write(&b, "#include \"a.h\"\n");

        let res = walk(std::slice::from_ref(&a), &[]);
        let ca = std::fs::canonicalize(&a).unwrap();
        let cb = std::fs::canonicalize(&b).unwrap();
        assert!(res.reached.contains(&ca));
        assert!(res.reached.contains(&cb));
    }

    #[test]
    fn w11_diamond_dedupes() {
        let tmp = TempDir::new().unwrap();
        let main = tmp.path().join("main.cpp");
        let a = tmp.path().join("a.h");
        let b = tmp.path().join("b.h");
        let common = tmp.path().join("common.h");
        write(&main, "#include \"a.h\"\n#include \"b.h\"\n");
        write(&a, "#include \"common.h\"\n");
        write(&b, "#include \"common.h\"\n");
        write(&common, "// common\n");

        let res = walk(std::slice::from_ref(&main), &[]);
        let cc = std::fs::canonicalize(&common).unwrap();
        let count = res.reached.iter().filter(|p| **p == cc).count();
        assert_eq!(count, 1);
    }

    #[test]
    fn w12_depth_5_chain() {
        let tmp = TempDir::new().unwrap();
        for i in 1..=5 {
            let next = if i == 5 {
                String::new()
            } else {
                format!("#include \"h{}.h\"\n", i + 1)
            };
            write(&tmp.path().join(format!("h{}.h", i)), &next);
        }
        let main = tmp.path().join("main.cpp");
        write(&main, "#include \"h1.h\"\n");
        let res = walk(std::slice::from_ref(&main), &[]);
        for i in 1..=5 {
            let p = std::fs::canonicalize(tmp.path().join(format!("h{}.h", i))).unwrap();
            assert!(res.reached.contains(&p), "missing h{}.h", i);
        }
    }

    #[test]
    fn w20_deterministic_order() {
        let tmp = TempDir::new().unwrap();
        let main = tmp.path().join("main.cpp");
        let z = tmp.path().join("z.h");
        let a = tmp.path().join("a.h");
        write(&z, "");
        write(&a, "");
        write(&main, "#include \"z.h\"\n#include \"a.h\"\n");
        let seeds = std::slice::from_ref(&main);
        let r1 = walk(seeds, &[]);
        let r2 = walk(seeds, &[]);
        assert_eq!(r1, r2);
    }
}
