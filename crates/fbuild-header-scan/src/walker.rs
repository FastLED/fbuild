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

use crate::scanner::{
    IncludeKind, IncludeRef, defined_macro_names, scan, scan_active, scan_active_with_known,
};

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
    resolver: IncludeResolver,
}

/// Memoized include resolution for one search-path list.
///
/// A name's search-path resolution does not depend on the including file, so
/// it is computed once instead of `is_file`-probing every search path for every
/// reference. That probing was two thirds of an incremental FastLED build's
/// daemon CPU with ~400 ESP32 search paths (FastLED/fbuild#1539).
#[derive(Debug, Default)]
struct IncludeResolver {
    search_paths: Vec<PathBuf>,
    by_name: HashMap<String, Option<PathBuf>>,
    canonical: HashMap<PathBuf, PathBuf>,
    /// First path component -> indices of the search paths whose root holds
    /// it, in search order. Built on the first miss; `Some(None)` means a root
    /// could not be listed and every lookup probes each path instead.
    roots: Option<Option<HashMap<String, Vec<usize>>>>,
}

/// Keys fold case so a case-insensitive filesystem's `Foo.h` still finds
/// `foo.h`. That only widens the candidate set; each candidate is confirmed
/// with `is_file` in search order, so the first match is the probe loop's.
fn root_key(name: &str) -> String {
    name.to_lowercase()
}

fn list_roots(search_paths: &[PathBuf]) -> Option<HashMap<String, Vec<usize>>> {
    let mut roots: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, dir) in search_paths.iter().enumerate() {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            // A missing search path holds nothing, exactly as probing it would.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) if !dir.is_dir() => continue,
            Err(_) => return None,
        };
        for entry in entries {
            let name = entry.ok()?.file_name().into_string().ok()?;
            let owners = roots.entry(root_key(&name)).or_default();
            if owners.last() != Some(&index) {
                owners.push(index);
            }
        }
    }
    Some(roots)
}

impl IncludeResolver {
    fn bind(&mut self, search_paths: &[PathBuf]) {
        if self.search_paths != search_paths {
            self.search_paths = search_paths.to_vec();
            self.by_name.clear();
            self.roots = None;
        }
    }

    fn resolve(&mut self, inc: &IncludeRef, from: &Path) -> Option<PathBuf> {
        if inc.kind == IncludeKind::Quoted {
            if let Some(parent) = from.parent() {
                let candidate = parent.join(&inc.path);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
        if let Some(found) = self.by_name.get(&inc.path) {
            return found.clone();
        }
        let found = self.search(&inc.path);
        self.by_name.insert(inc.path.clone(), found.clone());
        found
    }

    /// The first search path holding `name`. Only paths whose root lists the
    /// name's first component can hold it, so those are the only ones probed.
    fn search(&mut self, name: &str) -> Option<PathBuf> {
        let plain = !name.contains('\\')
            && !Path::new(name).is_absolute()
            && name
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != "..");
        let roots = self
            .roots
            .get_or_insert_with(|| list_roots(&self.search_paths));
        match (plain, roots) {
            (true, Some(roots)) => {
                let first = name.split('/').next().unwrap_or(name);
                let owners = roots.get(&root_key(first))?;
                owners
                    .iter()
                    .map(|&index| self.search_paths[index].join(name))
                    .find(|candidate| candidate.is_file())
            }
            _ => self
                .search_paths
                .iter()
                .map(|sp| sp.join(name))
                .find(|candidate| candidate.is_file()),
        }
    }

    fn canon(&mut self, path: &Path) -> PathBuf {
        if let Some(found) = self.canonical.get(path) {
            return found.clone();
        }
        let resolved = canon(path);
        self.canonical.insert(path.to_path_buf(), resolved.clone());
        resolved
    }
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

/// Walk the include graph using only active preprocessor branches.
///
/// `defines` must be the build's compiler defines. This is the LDF entry
/// point: headers behind a disabled branch do not become library dependencies.
pub fn walk_active(
    seeds: &[PathBuf],
    search_paths: &[PathBuf],
    defines: &HashMap<String, String>,
) -> WalkResult {
    let mut state = WalkState::new();
    walk_with_state_active(seeds, search_paths, defines, &mut state)
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
    walk_with_state_scanner(seeds, search_paths, state, &scan)
}

/// Active-branch counterpart to [`walk_with_state`].
pub fn walk_with_state_active(
    seeds: &[PathBuf],
    search_paths: &[PathBuf],
    defines: &HashMap<String, String>,
    state: &mut WalkState,
) -> WalkResult {
    walk_with_state_scanner(seeds, search_paths, state, &|src| scan_active(src, defines))
}

/// [`walk_with_state_active`] told which macro names the corpus defines.
///
/// See [`crate::scanner::scan_active_with_known`]: a guard on a macro the
/// project defines somewhere is undecidable from the command line alone, and
/// pruning it hid includes that genuinely compile (FastLED/fbuild#1371).
pub fn walk_with_state_active_known(
    seeds: &[PathBuf],
    search_paths: &[PathBuf],
    defines: &HashMap<String, String>,
    defined_somewhere: &HashSet<String>,
    state: &mut WalkState,
) -> WalkResult {
    walk_with_state_scanner(seeds, search_paths, state, &|src| {
        scan_active_with_known(src, defines, defined_somewhere)
    })
}

/// Collect every macro name `#define`d in any file reachable from `seeds`.
///
/// Walks textually (all branches), because the question is what the corpus
/// *could* define — a conditional must not filter the answer. Uses its own
/// [`WalkState`] so the active passes keep their own scan cache semantics.
pub fn collect_defined_macro_names(seeds: &[PathBuf], search_paths: &[PathBuf]) -> HashSet<String> {
    collect_defined_macro_names_with(seeds, search_paths, &mut WalkState::new())
}

/// [`collect_defined_macro_names`] reusing `state`'s include resolution.
///
/// Only the resolver is shared: the textual walk keeps its own scan cache and
/// visited set, so the active passes that follow on `state` are unaffected.
pub fn collect_defined_macro_names_with(
    seeds: &[PathBuf],
    search_paths: &[PathBuf],
    state: &mut WalkState,
) -> HashSet<String> {
    let mut textual = WalkState {
        resolver: std::mem::take(&mut state.resolver),
        ..WalkState::default()
    };
    let result = walk_with_state(seeds, search_paths, &mut textual);
    state.resolver = textual.resolver;
    // Parallel, like the walk's own reads: thousands of files on FastLED.
    result
        .reached
        .par_iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .map(|src| {
            defined_macro_names(&src)
                .into_iter()
                .collect::<HashSet<_>>()
        })
        .reduce(HashSet::new, |mut all, names| {
            all.extend(names);
            all
        })
}

fn walk_with_state_scanner<F>(
    seeds: &[PathBuf],
    search_paths: &[PathBuf],
    state: &mut WalkState,
    scanner: &F,
) -> WalkResult
where
    F: Fn(&str) -> Vec<IncludeRef> + Sync,
{
    tracing::debug!(
        seeds = seeds.len(),
        search_paths = search_paths.len(),
        "ldf_walk"
    );
    let mut reached: BTreeSet<PathBuf> = BTreeSet::new();
    let mut unresolved: BTreeSet<String> = BTreeSet::new();
    let mut frontier: VecDeque<PathBuf> = VecDeque::new();
    state.resolver.bind(search_paths);

    for seed in seeds {
        let canon = state.resolver.canon(seed);
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
                    Some((p.clone(), scanner(&text)))
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
                match state.resolver.resolve(inc, file) {
                    Some(resolved) => {
                        let canon = state.resolver.canon(&resolved);
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

fn canon(p: &Path) -> PathBuf {
    // FastLED/fbuild#844 sync-context allowlist: this helper runs inside
    // a rayon-parallel BFS (`walk_with_state`). Using
    // `fbuild_core::path::canonicalize_existing(...).await` would force
    // the walker async, which would cascade through every downstream LDF
    // caller. File is allowlisted in
    // `dylints/ban_std_fs_canonicalize/src/allowlist.txt`.
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn tempdir() -> TempDir {
        TempDir::new_in(fbuild_paths::temp_subdir("fbuild-header-scan-tests")).unwrap()
    }

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

    #[test]
    fn w01_quoted_resolves_same_dir_first() {
        let tmp = tempdir();
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
        let tmp = tempdir();
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
        let tmp = tempdir();
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
        let tmp = tempdir();
        let main = tmp.path().join("main.cpp");
        write(&main, "#include <does_not_exist.h>\n");
        let res = walk(std::slice::from_ref(&main), &[]);
        assert!(res.unresolved.iter().any(|s| s == "does_not_exist.h"));
    }

    #[test]
    fn w10_cycle_terminates() {
        let tmp = tempdir();
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
        let tmp = tempdir();
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
        let tmp = tempdir();
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
        let tmp = tempdir();
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

    #[test]
    fn w30_cached_search_result_never_overrides_a_sibling_quoted_include() {
        let tmp = tempdir();
        let inc = tmp.path().join("inc");
        write(&inc.join("x.h"), "");
        // a/main.cpp has its own sibling x.h; b/main.cpp does not.
        let a_main = tmp.path().join("a/main.cpp");
        let b_main = tmp.path().join("b/main.cpp");
        write(&tmp.path().join("a/x.h"), "");
        write(&a_main, "#include \"x.h\"\n");
        write(&b_main, "#include \"x.h\"\n#include <x.h>\n");

        let res = walk(&[b_main, a_main], std::slice::from_ref(&inc));

        let canon = |p: PathBuf| std::fs::canonicalize(p).unwrap();
        assert!(res.reached.contains(&canon(tmp.path().join("a/x.h"))));
        assert!(res.reached.contains(&canon(inc.join("x.h"))));
    }

    #[test]
    fn w31_reused_state_with_new_search_paths_resolves_afresh() {
        let tmp = tempdir();
        let first = tmp.path().join("first");
        let second = tmp.path().join("second");
        write(&second.join("only_second.h"), "");
        let main = tmp.path().join("src/main.cpp");
        write(&main, "#include <only_second.h>\n");
        let seeds = std::slice::from_ref(&main);
        let mut state = WalkState::new();

        let res = walk_with_state(seeds, std::slice::from_ref(&first), &mut state);
        assert_eq!(res.unresolved, ["only_second.h"]);

        let mut fresh = WalkState {
            resolver: std::mem::take(&mut state.resolver),
            ..WalkState::default()
        };
        let res = walk_with_state(seeds, &[first, second.clone()], &mut fresh);
        assert!(res.unresolved.is_empty());
        assert!(
            res.reached
                .contains(&std::fs::canonicalize(second.join("only_second.h")).unwrap())
        );
    }

    #[test]
    fn w32_first_component_in_several_roots_keeps_search_order() {
        let tmp = tempdir();
        let (one, two, three) = (
            tmp.path().join("one"),
            tmp.path().join("two"),
            tmp.path().join("three"),
        );
        std::fs::create_dir_all(one.join("a")).unwrap();
        write(&two.join("a/x.h"), "");
        write(&three.join("a/x.h"), "");
        let main = tmp.path().join("src/main.cpp");
        write(&main, "#include <a/x.h>\n");

        let res = walk(std::slice::from_ref(&main), &[one, two.clone(), three]);

        assert!(
            res.reached
                .contains(&std::fs::canonicalize(two.join("a/x.h")).unwrap())
        );
        assert!(res.unresolved.is_empty());
    }

    #[test]
    fn w33_dot_dot_names_still_resolve() {
        let tmp = tempdir();
        let inc = tmp.path().join("inc");
        std::fs::create_dir_all(inc.join("sub")).unwrap();
        write(&inc.join("up.h"), "");
        let main = tmp.path().join("src/main.cpp");
        write(&main, "#include <sub/../up.h>\n");

        let res = walk(std::slice::from_ref(&main), std::slice::from_ref(&inc));

        assert!(
            res.reached
                .contains(&std::fs::canonicalize(inc.join("up.h")).unwrap())
        );
    }

    #[test]
    fn w34_missing_search_path_is_skipped() {
        let tmp = tempdir();
        let inc = tmp.path().join("inc");
        write(&inc.join("x.h"), "");
        let main = tmp.path().join("src/main.cpp");
        write(&main, "#include <x.h>\n");

        let res = walk(
            std::slice::from_ref(&main),
            &[tmp.path().join("absent"), inc.clone()],
        );

        assert!(
            res.reached
                .contains(&std::fs::canonicalize(inc.join("x.h")).unwrap())
        );
    }
}
