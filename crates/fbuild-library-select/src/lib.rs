//! PlatformIO-LDF-style library resolver.
//!
//! Given a set of seed source files (the project's `src/`, `lib/`, `include/`
//! trees), a list of discovered framework libraries, and the project's include
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

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use fbuild_header_scan::walk;
use fbuild_packages::library::FrameworkLibrary;

/// Resolved library selection plus the transitive include closure.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
/// `seeds` are the source files to walk from (sketch, project `src/`,
/// `include/`, `lib/` trees).
/// `project_search_paths` are the project's own include roots — consulted for
/// `<...>` includes before framework libs.
/// `libraries` is the full set of framework libraries discovered under the
/// framework's `libraries/` dir.
pub fn resolve(
    seeds: &[PathBuf],
    project_search_paths: &[PathBuf],
    libraries: &[FrameworkLibrary],
) -> Selection {
    let mut selected: BTreeSet<usize> = BTreeSet::new();
    let mut all_included: BTreeSet<PathBuf> = BTreeSet::new();
    let mut all_unresolved: BTreeSet<String> = BTreeSet::new();

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

    // Pass 1: BFS from project seeds.
    let res = walk(seeds, &full_search_paths);
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

    // Pass 2: reconciliation. Re-walk with each selected library's full source
    // set as seeds, in case a lib-to-lib dep is only visible through a `.cpp`
    // (not a header). Keeps iterating until the selection stabilizes, which for
    // realistic Arduino-library graphs is 1–2 rounds.
    loop {
        let mut recon_seeds: Vec<PathBuf> = seeds.to_vec();
        for idx in &selected {
            for src in &libraries[*idx].source_files {
                recon_seeds.push(src.clone());
            }
        }
        let res = walk(&recon_seeds, &full_search_paths);
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

    Selection {
        included_files: all_included.into_iter().collect(),
        required_libraries,
        source_files: source_files.into_iter().collect(),
        include_dirs: include_dirs.into_keys().collect(),
        unresolved: all_unresolved.into_iter().collect(),
    }
}

fn canon(p: &Path) -> PathBuf {
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

    #[test]
    fn r01_direct_include_selects_library() {
        let tmp = TempDir::new().unwrap();
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

    #[test]
    fn r02_transitive_library_selection() {
        let tmp = TempDir::new().unwrap();
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
    fn r03_no_includes_selects_nothing() {
        let tmp = TempDir::new().unwrap();
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
        let tmp = TempDir::new().unwrap();
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
        let tmp = TempDir::new().unwrap();
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
        let tmp = TempDir::new().unwrap();
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
        let tmp = TempDir::new().unwrap();
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
        let tmp = TempDir::new().unwrap();
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
        let tmp = TempDir::new().unwrap();
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
}
