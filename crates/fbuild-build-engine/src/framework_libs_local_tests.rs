//! Tests for [`super::select_local_libraries`] — which project `lib/`
//! libraries a build compiles (FastLED/fbuild#1410).
//!
//! Kept apart from `framework_libs_tests.rs` to stay under the workspace
//! 1000-LOC limit.

use super::*;

/// Lay out `lib/<name>/src/<name>.{h,cpp}` with the given `.cpp` body.
fn write_local_lib(project: &Path, name: &str, cpp: &str) {
    let src = project.join("lib").join(name).join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join(format!("{name}.h")), "").unwrap();
    std::fs::write(src.join(format!("{name}.cpp")), cpp).unwrap();
}

fn selected_names(project: &Path, src: &Path, declared: &[String]) -> Vec<String> {
    select_local_libraries(project, src, declared)
        .into_iter()
        .map(|library| library.name)
        .collect()
}

/// FastLED/fbuild#1410: the report's shape. `lib/` holds FastLED and a
/// SAMD-only library; the sketch includes only FastLED, so the SAMD library
/// must not be compiled for an Uno.
#[test]
fn local_library_nothing_includes_is_not_selected() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project = tmp.path();
    let src = project.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("main.ino"), "#include <FastLED.h>\n").unwrap();
    write_local_lib(project, "FastLED", "#include \"FastLED.h\"\n");
    write_local_lib(
        project,
        "Adafruit_ZeroDMA",
        "#include <malloc.h>\n#include \"Adafruit_ZeroDMA.h\"\n",
    );

    assert_eq!(selected_names(project, &src, &[]), vec!["FastLED"]);
}

/// A library reached only through another local library's source is part of
/// the link, so it must still be selected.
#[test]
fn local_library_reached_through_another_local_library_is_selected() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project = tmp.path();
    let src = project.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("main.cpp"), "#include <A.h>\n").unwrap();
    write_local_lib(project, "A", "#include <B.h>\n");
    write_local_lib(project, "B", "");
    write_local_lib(project, "C", "");

    assert_eq!(selected_names(project, &src, &[]), vec!["A", "B"]);
}

/// A guard on a compiler-builtin macro is invisible to the scanner. Pruning
/// it would drop a library the compiler includes, so every arm is scanned.
#[test]
fn local_library_behind_builtin_macro_guard_is_selected() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project = tmp.path();
    let src = project.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("main.cpp"),
        "#if defined(__XTENSA__)\n#include <Xtensa.h>\n#endif\n",
    )
    .unwrap();
    write_local_lib(project, "Xtensa", "");

    assert_eq!(selected_names(project, &src, &[]), vec!["Xtensa"]);
}

/// `lib_deps` naming a local library selects it even though no include
/// reaches it — PlatformIO's explicit-dependency escape hatch.
#[test]
fn lib_deps_selects_an_unincluded_local_library() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project = tmp.path();
    let src = project.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("main.cpp"), "int main() {}\n").unwrap();
    write_local_lib(project, "Weak", "");

    assert!(selected_names(project, &src, &[]).is_empty());
    assert_eq!(
        selected_names(project, &src, &["Weak".to_string()]),
        vec!["Weak"]
    );
}

/// Without a `src/`, the project directory is the sketch root. Walking it
/// must not turn every `lib/` source into a seed, or every library is
/// selected again.
#[test]
fn sketch_at_project_root_does_not_seed_from_lib() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project = tmp.path();
    std::fs::write(project.join("sketch.ino"), "#include <Used.h>\n").unwrap();
    write_local_lib(project, "Used", "");
    write_local_lib(project, "Unused", "");

    assert_eq!(selected_names(project, project, &[]), vec!["Used"]);
}

/// An unselected local library is not compiled, so its sources must not
/// select a framework library either ("what compiles is what seeds").
#[test]
fn unselected_local_library_source_cannot_select_framework_library() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project = tmp.path().join("project");
    let src = project.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("main.cpp"), "int main() {}\n").unwrap();
    write_local_lib(&project, "Unused", "#include <SPI.h>\n");

    let spi = tmp.path().join("framework").join("libraries").join("SPI");
    std::fs::create_dir_all(&spi).unwrap();
    std::fs::write(spi.join("SPI.h"), "").unwrap();
    std::fs::write(spi.join("SPI.cpp"), "").unwrap();
    let libraries = vec![FrameworkLibrary {
        name: "SPI".to_string(),
        dir: spi.clone(),
        include_dirs: vec![spi.clone()],
        source_files: vec![spi.join("SPI.cpp")],
    }];

    let sources =
        resolve_framework_library_sources_active(&libraries, &project, &src, &HashMap::new());
    assert!(sources.is_empty(), "{sources:?}");
}
