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

/// Collect every buildable source file from a library, honoring the
/// [Arduino library specification](https://arduino.github.io/arduino-cli/1.5/library-specification/).
///
/// - **1.5 recursive layout** (library has `src/`): scan `src/**` recursively.
///   The library root is *not* scanned. This is the modern layout used by
///   well-organized libraries.
/// - **1.0 flat layout** (no `src/`): scan the library root *non-recursively*
///   plus the literal `utility/` subdirectory recursively. Every other
///   subdirectory (`fontconvert/`, `util/`, `Fonts/`, `examples/`, `extras/`,
///   etc.) is ignored.
///
/// The flat-layout rule is what protects fbuild from compiling host-only
/// tools that ship inside libraries — e.g. `ssd1351/fontconvert/fontconvert.c`
/// (a libfreetype-linked desktop utility) under
/// `framework-arduinoteensy/libraries/ssd1351/`. Arduino IDE and PlatformIO
/// skip those because the spec says to scan only root + `utility/`; fbuild
/// previously walked the full tree and tried to ARM-cross-compile the host
/// tool, which fails at `#include <ft2build.h>`. See FastLED/fbuild#267.
pub fn collect_library_sources(lib_dir: &Path) -> Vec<PathBuf> {
    let mut sources = Vec::new();
    let src = lib_dir.join("src");
    if src.is_dir() {
        // 1.5 layout — recursive scan of src/ only.
        collect_library_sources_inner(&src, &mut sources);
    } else {
        // 1.0 flat layout per Arduino spec:
        //   * root level (non-recursive)
        //   * utility/ (recursive, literal lowercase per the spec)
        collect_root_level_sources(lib_dir, &mut sources);
        let utility = lib_dir.join("utility");
        if utility.is_dir() {
            collect_library_sources_inner(&utility, &mut sources);
        }
    }
    sources.sort();
    sources
}

/// Non-recursive scan of a directory for buildable source files.
/// Used only for the root of a 1.0 flat-layout library.
fn collect_root_level_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && is_buildable_source(&path) {
            out.push(path);
        }
    }
}

/// Recursive scan used inside `src/` (1.5 layout) and `utility/` (1.0 layout).
/// Still skips `examples/`, `tests/`, and `extras/` defensively in case a
/// library nests them under `src/` or `utility/`.
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
        } else if is_buildable_source(&path) {
            out.push(path);
        }
    }
}

fn is_buildable_source(path: &Path) -> bool {
    let ext = path
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    matches!(ext.as_str(), "c" | "cpp" | "cc" | "cxx" | "s")
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

    /// Regression for FastLED/fbuild#267 — 1.0 flat-layout libraries
    /// (no `src/`) must scan only the library root non-recursively plus
    /// the literal `utility/` subdirectory. Subdirectories like
    /// `fontconvert/`, `util/`, `Fonts/`, and `examples/` are ignored
    /// per the Arduino library spec.
    ///
    /// The specific failure this guards: `ssd1351/fontconvert/fontconvert.c`
    /// is a desktop host tool linking `-lfreetype`; ARM-cross-compiling it
    /// fails on `#include <ft2build.h>`. Arduino IDE / PlatformIO skip
    /// `fontconvert/` because the spec says scan root + `utility/` only.
    #[test]
    fn collect_library_sources_flat_layout_arduino_spec() {
        let tmp = tempfile::TempDir::new().unwrap();
        let lib = tmp.path().join("ssd1351");
        std::fs::create_dir_all(&lib).unwrap();

        // Root level — compiled.
        std::fs::write(lib.join("ssd1351.cpp"), "").unwrap();
        std::fs::write(lib.join("ssd1351.h"), "").unwrap(); // header, not source

        // `utility/` — compiled (recursive).
        std::fs::create_dir_all(lib.join("utility")).unwrap();
        std::fs::write(lib.join("utility").join("helpers.cpp"), "").unwrap();
        std::fs::create_dir_all(lib.join("utility").join("nested")).unwrap();
        std::fs::write(lib.join("utility").join("nested").join("deep.cpp"), "").unwrap();

        // Non-standard subdirs — ALL must be skipped.
        std::fs::create_dir_all(lib.join("fontconvert")).unwrap();
        std::fs::write(
            lib.join("fontconvert").join("fontconvert.c"),
            "#include <ft2build.h>\n", // would fail to ARM-cross-compile
        )
        .unwrap();
        std::fs::create_dir_all(lib.join("util")).unwrap();
        std::fs::write(lib.join("util").join("misc.cpp"), "").unwrap();
        std::fs::create_dir_all(lib.join("Fonts")).unwrap();
        std::fs::write(lib.join("Fonts").join("Roboto.c"), "").unwrap();
        std::fs::create_dir_all(lib.join("examples").join("Demo")).unwrap();
        std::fs::write(lib.join("examples").join("Demo").join("Demo.ino"), "").unwrap();

        let sources = collect_library_sources(&lib);

        let mut expected = vec![
            lib.join("ssd1351.cpp"),
            lib.join("utility").join("helpers.cpp"),
            lib.join("utility").join("nested").join("deep.cpp"),
        ];
        expected.sort();

        assert_eq!(
            sources, expected,
            "1.0 flat layout must yield ONLY root non-recursive + utility/ \
             recursive per Arduino spec — see #267. \
             Got {sources:?}, expected {expected:?}"
        );
    }

    /// Regression for FastLED/fbuild#267 — 1.5 recursive layout (library
    /// has `src/`) must scan only `src/**`. Any root-level source files
    /// are intentionally ignored per the Arduino library spec; the
    /// library's `library.properties` declares it as 1.5 by virtue of
    /// shipping `src/`.
    #[test]
    fn collect_library_sources_recursive_layout_arduino_spec() {
        let tmp = tempfile::TempDir::new().unwrap();
        let lib = tmp.path().join("SPI");
        std::fs::create_dir_all(lib.join("src")).unwrap();
        std::fs::create_dir_all(lib.join("src").join("sub")).unwrap();

        // src/ — recursive scan.
        std::fs::write(lib.join("src").join("SPI.cpp"), "").unwrap();
        std::fs::write(lib.join("src").join("sub").join("bar.cpp"), "").unwrap();

        // Root-level — must NOT be compiled (1.5 layout ignores root).
        std::fs::write(lib.join("baz.cpp"), "").unwrap();

        // examples/, tests/ — always skipped.
        std::fs::create_dir_all(lib.join("examples")).unwrap();
        std::fs::write(lib.join("examples").join("Demo.cpp"), "").unwrap();

        let sources = collect_library_sources(&lib);

        let mut expected = vec![
            lib.join("src").join("SPI.cpp"),
            lib.join("src").join("sub").join("bar.cpp"),
        ];
        expected.sort();

        assert_eq!(
            sources, expected,
            "1.5 recursive layout must yield ONLY src/** per Arduino spec — \
             root-level `baz.cpp` must be ignored — see #267. \
             Got {sources:?}, expected {expected:?}"
        );
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
