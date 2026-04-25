//! Transitive include-graph walker.
//!
//! Given a set of seed source files and an ordered list of search paths, walks
//! every reachable `#include` and returns the set of resolved files (sorted)
//! plus the set of include strings that could not be resolved. The walker is
//! BFS over a visited set so cycles, diamonds, and arbitrary depth all
//! terminate correctly.

use std::collections::{BTreeSet, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use crate::scanner::{scan, IncludeKind, IncludeRef};

/// Result of a walk. `reached` and `unresolved` are sorted for deterministic
/// cache keys.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WalkResult {
    pub reached: Vec<PathBuf>,
    pub unresolved: Vec<String>,
}

/// Walk the include graph starting from `seeds` over `search_paths`.
///
/// `search_paths` is consulted in order for `<...>` includes and as a
/// secondary lookup for `"..."` includes (after the same-directory check).
/// A file is added to `reached` exactly once. Files outside `search_paths`
/// are still reached if they are seeds or `"..."`-resolved relative to a
/// seed/visited file.
pub fn walk(seeds: &[PathBuf], search_paths: &[PathBuf]) -> WalkResult {
    let mut reached: BTreeSet<PathBuf> = BTreeSet::new();
    let mut unresolved: BTreeSet<String> = BTreeSet::new();
    let mut visited: HashSet<PathBuf> = HashSet::new();
    let mut queue: VecDeque<PathBuf> = VecDeque::new();

    for seed in seeds {
        let canon = canon(seed);
        if visited.insert(canon.clone()) {
            queue.push_back(canon.clone());
            reached.insert(canon);
        }
    }

    while let Some(file) = queue.pop_front() {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for inc in scan(&text) {
            match resolve(&inc, &file, search_paths) {
                Some(resolved) => {
                    let canon = canon(&resolved);
                    if visited.insert(canon.clone()) {
                        reached.insert(canon.clone());
                        queue.push_back(canon);
                    }
                }
                None => {
                    unresolved.insert(inc.path.clone());
                }
            }
        }
    }

    WalkResult {
        reached: reached.into_iter().collect(),
        unresolved: unresolved.into_iter().collect(),
    }
}

fn resolve(inc: &IncludeRef, from: &Path, search_paths: &[PathBuf]) -> Option<PathBuf> {
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
