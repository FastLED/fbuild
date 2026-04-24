//! Shared model for a bundled Arduino-style framework library.
//!
//! Frameworks such as Teensyduino and STM32duino ship a `libraries/` directory
//! whose subdirectories each contain an Arduino library (e.g. `SPI`, `Wire`).
//! Every library exposes its own include dirs and source files. The types and
//! helpers here walk that directory layout so each platform's build
//! orchestrator can discover the libraries a sketch actually uses.

use std::path::{Path, PathBuf};

/// A bundled framework library discovered under `libraries/<name>/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameworkLibrary {
    pub name: String,
    pub dir: PathBuf,
    pub include_dirs: Vec<PathBuf>,
    pub source_files: Vec<PathBuf>,
}

/// Enumerate every library under `libraries_dir`.
///
/// Returns libraries sorted by name; missing `libraries_dir` yields an empty
/// vec.
pub fn discover_framework_libraries(libraries_dir: &Path) -> Vec<FrameworkLibrary> {
    let mut libs = Vec::new();
    let Ok(entries) = std::fs::read_dir(libraries_dir) else {
        return libs;
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
            .to_string();
        if name.is_empty() {
            continue;
        }
        libs.push(FrameworkLibrary {
            name,
            include_dirs: library_include_dirs(&dir),
            source_files: collect_library_sources(&dir),
            dir,
        });
    }

    libs.sort_by(|a, b| a.name.cmp(&b.name));
    libs
}

/// Resolve the include search paths for a single library.
///
/// Follows the Arduino library layout: prefer `src/` if present, otherwise
/// fall back to the library root. `utility/` and `include/` are added when
/// present to cover older / less-standard layouts.
pub fn library_include_dirs(lib_dir: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let src = lib_dir.join("src");
    if src.is_dir() {
        dirs.push(src);
    } else {
        dirs.push(lib_dir.to_path_buf());
    }

    let utility = lib_dir.join("utility");
    if utility.is_dir() {
        dirs.push(utility);
    }
    let include = lib_dir.join("include");
    if include.is_dir() {
        dirs.push(include);
    }
    dirs
}

/// Collect every buildable source file from a library.
///
/// Skips `examples/`, `tests/`, and `extras/` subtrees to keep user-facing
/// demos out of the build.
pub fn collect_library_sources(lib_dir: &Path) -> Vec<PathBuf> {
    let search_dir = {
        let src = lib_dir.join("src");
        if src.is_dir() {
            src
        } else {
            lib_dir.to_path_buf()
        }
    };

    let mut sources = Vec::new();
    collect_library_sources_inner(&search_dir, &mut sources);
    sources.sort();
    sources
}

fn collect_library_sources_inner(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase();
            if matches!(
                name.as_str(),
                "example" | "examples" | "test" | "tests" | "extras"
            ) {
                continue;
            }
            collect_library_sources_inner(&path, out);
        } else {
            let ext = path
                .extension()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase();
            if matches!(ext.as_str(), "c" | "cpp" | "cc" | "cxx" | "s") {
                out.push(path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_include_dirs_prefers_src() {
        let tmp = tempfile::TempDir::new().unwrap();
        let lib = tmp.path().join("SPI");
        std::fs::create_dir_all(lib.join("src")).unwrap();
        assert_eq!(library_include_dirs(&lib), vec![lib.join("src")]);
    }

    #[test]
    fn library_include_dirs_falls_back_to_root() {
        let tmp = tempfile::TempDir::new().unwrap();
        let lib = tmp.path().join("SPI");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(lib.join("SPI.h"), "").unwrap();
        assert_eq!(library_include_dirs(&lib), vec![lib]);
    }

    #[test]
    fn collect_library_sources_skips_examples_and_extras() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("OctoWS2811.cpp"), "").unwrap();
        std::fs::create_dir_all(tmp.path().join("examples")).unwrap();
        std::fs::write(tmp.path().join("examples").join("Demo.cpp"), "").unwrap();
        std::fs::create_dir_all(tmp.path().join("extras")).unwrap();
        std::fs::write(tmp.path().join("extras").join("tool.c"), "").unwrap();

        let sources = collect_library_sources(tmp.path());
        assert_eq!(sources, vec![tmp.path().join("OctoWS2811.cpp")]);
    }

    #[test]
    fn discover_framework_libraries_walks_each_subdirectory() {
        let tmp = tempfile::TempDir::new().unwrap();
        let libs_dir = tmp.path().join("libraries");
        let spi = libs_dir.join("SPI").join("src");
        std::fs::create_dir_all(&spi).unwrap();
        std::fs::write(spi.join("SPI.h"), "").unwrap();
        std::fs::write(spi.join("SPI.cpp"), "").unwrap();

        let wire = libs_dir.join("Wire");
        std::fs::create_dir_all(&wire).unwrap();
        std::fs::write(wire.join("Wire.h"), "").unwrap();

        let libs = discover_framework_libraries(&libs_dir);
        assert_eq!(libs.len(), 2);
        assert_eq!(libs[0].name, "SPI");
        assert_eq!(libs[0].include_dirs, vec![spi.clone()]);
        assert_eq!(libs[0].source_files, vec![spi.join("SPI.cpp")]);
        assert_eq!(libs[1].name, "Wire");
    }

    #[test]
    fn discover_framework_libraries_missing_dir_is_empty() {
        let tmp = tempfile::TempDir::new().unwrap();
        let libs = discover_framework_libraries(&tmp.path().join("missing"));
        assert!(libs.is_empty());
    }
}
