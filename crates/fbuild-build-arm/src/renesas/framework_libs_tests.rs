//! Tests for bundled Arduino-library selection (FastLED/fbuild#2020).

use std::collections::HashMap;

use fbuild_core::path::NormalizedPath;
use fbuild_packages::library::FrameworkLibrary;

use super::{log_selection, select_bundled_libraries};

const LIBS: [&str; 3] = ["WiFiS3", "SPI", "Arduino_FreeRTOS"];

/// Synthetic framework tree shaped like the ArduinoCore-renesas core:
/// `libraries/<name>/src/<name>.h` plus one uniquely named `.cpp` per library,
/// and a project holding a single sketch.
struct Fixture {
    _dir: tempfile::TempDir,
    project_dir: NormalizedPath,
    src_dir: NormalizedPath,
    libraries: Vec<FrameworkLibrary>,
    include_dirs: Vec<NormalizedPath>,
}

impl Fixture {
    fn new(sketch: &str) -> Self {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().to_path_buf();
        let libraries_dir = root.join("framework").join("libraries");
        let mut libraries = Vec::new();
        let mut include_dirs = Vec::new();
        for name in LIBS {
            let lib_dir = libraries_dir.join(name);
            let src = lib_dir.join("src");
            std::fs::create_dir_all(&src).unwrap();
            std::fs::write(
                src.join(format!("{name}.h")),
                format!("#pragma once\nclass {name}Class {{ public: void begin(); }};\n"),
            )
            .unwrap();
            let cpp = src.join(format!("{name}Impl.cpp"));
            std::fs::write(
                &cpp,
                format!("#include \"{name}.h\"\nvoid {name}Class::begin() {{}}\n"),
            )
            .unwrap();
            include_dirs.push(NormalizedPath::new(&src));
            libraries.push(FrameworkLibrary {
                name: name.to_string(),
                include_dirs: vec![src],
                source_files: vec![cpp],
                dir: lib_dir,
            });
        }
        let project_dir = root.join("project");
        let src_dir = project_dir.join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(src_dir.join("main.cpp"), sketch).unwrap();

        Self {
            _dir: dir,
            project_dir: NormalizedPath::new(&project_dir),
            src_dir: NormalizedPath::new(&src_dir),
            libraries,
            include_dirs,
        }
    }

    fn select(
        &self,
        defines: &HashMap<String, String>,
        declared: &[String],
    ) -> super::BundledLibraries {
        select_bundled_libraries(
            &self.libraries,
            self.project_dir.as_path(),
            self.src_dir.as_path(),
            defines,
            declared,
            &self.include_dirs,
        )
        .unwrap()
    }
}

fn has_source(bundled: &super::BundledLibraries, name: &str) -> bool {
    bundled
        .sources
        .iter()
        .any(|source| source.as_path().file_name().and_then(|f| f.to_str()) == Some(name))
}

fn has_include_dir(bundled: &super::BundledLibraries, dir_name: &str) -> bool {
    bundled.include_dirs.iter().any(|dir| {
        dir.as_path()
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|f| f.to_str())
            == Some(dir_name)
    })
}

/// A sketch that includes `WiFiS3.h` must get the library's include dir on the
/// compiler's path and its sources in the compile set — the defect in
/// FastLED/fbuild#2020 was that neither happened.
#[test]
fn sketch_include_selects_the_bundled_library() {
    let fixture = Fixture::new("#include <WiFiS3.h>\nvoid setup() {}\n");
    let bundled = fixture.select(&HashMap::new(), &[]);

    assert_eq!(bundled.names, vec!["WiFiS3".to_string()]);
    assert!(
        has_source(&bundled, "WiFiS3Impl.cpp"),
        "expected WiFiS3Impl.cpp in {:?}",
        bundled.sources
    );
    assert!(
        has_include_dir(&bundled, "WiFiS3"),
        "expected the WiFiS3 src dir in {:?}",
        bundled.include_dirs
    );
    assert!(!has_source(&bundled, "SPIImpl.cpp"));
    assert!(!has_source(&bundled, "Arduino_FreeRTOSImpl.cpp"));
    assert!(!has_include_dir(&bundled, "Arduino_FreeRTOS"));
}

/// A sketch that reaches no bundled library compiles none of them — a project
/// must never regress into "compile all 23 bundled libraries".
#[test]
fn sketch_without_bundled_includes_selects_nothing() {
    let fixture = Fixture::new("void setup() {}\nvoid loop() {}\n");
    let bundled = fixture.select(&HashMap::new(), &[]);

    assert!(bundled.sources.is_empty(), "{:?}", bundled.sources);
    assert!(
        bundled.include_dirs.is_empty(),
        "{:?}",
        bundled.include_dirs
    );
    assert!(bundled.names.is_empty(), "{:?}", bundled.names);
}

/// An include whose guard evaluates false must not pull a library in.
#[test]
fn include_in_inactive_branch_is_not_selected() {
    let fixture = Fixture::new(
        "#define FB_USE_WIFI 0\n#if FB_USE_WIFI\n#include <WiFiS3.h>\n#endif\nvoid setup() {}\n",
    );
    let bundled = fixture.select(&HashMap::new(), &[]);

    assert!(bundled.sources.is_empty(), "{:?}", bundled.sources);
}

/// A guard that the project's own defines make true selects the library, so
/// board/`build_flags` defines reach the finder.
#[test]
fn include_in_active_defined_branch_is_selected() {
    let mut defines = HashMap::new();
    defines.insert("FB_USE_WIFI".to_string(), "1".to_string());
    let fixture = Fixture::new("#if FB_USE_WIFI\n#include <WiFiS3.h>\n#endif\nvoid setup() {}\n");
    let bundled = fixture.select(&defines, &[]);

    assert_eq!(bundled.names, vec!["WiFiS3".to_string()]);
}

/// `lib_deps` naming a bundled library is an explicit opt-in — the escape
/// hatch when the include walk cannot infer the dependency.
#[test]
fn declared_lib_deps_is_honored() {
    let fixture = Fixture::new("void setup() {}\n");
    let bundled = fixture.select(&HashMap::new(), &["SPI".to_string()]);

    assert_eq!(bundled.names, vec!["SPI".to_string()]);
    assert!(has_source(&bundled, "SPIImpl.cpp"));
    assert!(has_include_dir(&bundled, "SPI"));
}

/// A framework without a `libraries/` directory (custom cores,
/// `platform_packages` overrides) resolves to "nothing to compile", never an
/// error.
#[test]
fn no_libraries_available_is_not_an_error() {
    let dir = tempfile::TempDir::new().unwrap();
    let project = dir.path().join("project");
    let src = project.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("main.cpp"), "int main() { return 0; }\n").unwrap();

    let bundled = select_bundled_libraries(&[], &project, &src, &HashMap::new(), &[], &[]).unwrap();
    assert!(bundled.sources.is_empty());
    log_selection(&bundled);
}

/// Both logging paths run without panicking.
#[test]
fn log_selection_handles_both_paths() {
    let empty = super::BundledLibraries::default();
    log_selection(&empty);
    let fixture = Fixture::new("#include <WiFiS3.h>\n");
    log_selection(&fixture.select(&HashMap::new(), &[]));
}

/// Selected sources are always the library's own files, never the sketch.
#[test]
fn selection_never_contains_project_sources() {
    let fixture = Fixture::new("#include <WiFiS3.h>\nvoid setup() {}\n");
    let bundled = fixture.select(&HashMap::new(), &[]);

    assert!(!has_source(&bundled, "main.cpp"));
    assert_eq!(bundled.sources.len(), 1);
}
