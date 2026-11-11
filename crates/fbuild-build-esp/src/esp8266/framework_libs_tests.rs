//! Tests for bundled Arduino-library selection (FastLED/fbuild#1994).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use fbuild_packages::library::FrameworkLibrary;

use super::{log_selection, select_bundled_libraries};

/// Build a synthetic framework tree shaped like the Arduino ESP8266 core.
///
/// `libraries/<name>/src/<name>.h` + one `.cpp` per library, and a project
/// with a single sketch whose include list is `sketch_includes`.
struct Fixture {
    _dir: tempfile::TempDir,
    project_dir: PathBuf,
    src_dir: PathBuf,
    libraries: Vec<FrameworkLibrary>,
    include_dirs: Vec<PathBuf>,
}

impl Fixture {
    fn new(sketch: &str) -> Self {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().to_path_buf();
        let libraries_dir = root.join("framework").join("libraries");
        for name in ["ESP8266WiFi", "ESP8266WebServer", "UnusedLib"] {
            let src = libraries_dir.join(name).join("src");
            std::fs::create_dir_all(&src).unwrap();
            std::fs::write(
                src.join(format!("{name}.h")),
                format!("#pragma once\nclass {name}Class {{ public: void begin(); }};\n"),
            )
            .unwrap();
            std::fs::write(
                src.join(format!("{name}Impl.cpp")),
                format!("#include \"{name}.h\"\nvoid {name}Class::begin() {{}}\n"),
            )
            .unwrap();
        }
        let project_dir = root.join("project");
        let src_dir = project_dir.join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(src_dir.join("main.ino.cpp"), sketch).unwrap();

        let mut include_dirs: Vec<PathBuf> = Vec::new();
        let mut libraries = Vec::new();
        for name in ["ESP8266WiFi", "ESP8266WebServer", "UnusedLib"] {
            let lib_dir = libraries_dir.join(name);
            let src = lib_dir.join("src");
            include_dirs.push(src.clone());
            libraries.push(FrameworkLibrary {
                name: name.to_string(),
                include_dirs: vec![src.clone()],
                source_files: vec![src.join(format!("{name}Impl.cpp"))],
                dir: lib_dir,
            });
        }

        Self {
            _dir: dir,
            project_dir,
            src_dir,
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
            &self.project_dir,
            &self.src_dir,
            defines,
            declared,
            &self.include_dirs,
        )
        .unwrap()
    }

    fn file_name_is_source(sources: &[PathBuf], name: &str) -> bool {
        sources
            .iter()
            .any(|source| source.file_name().and_then(|f| f.to_str()) == Some(name))
    }
}

/// A sketch that includes a bundled library must compile that library's
/// sources — the defect in FastLED/fbuild#1994 was an empty selection here.
#[test]
fn sketch_include_selects_the_bundled_library_sources() {
    let fixture = Fixture::new("#include <ESP8266WiFi.h>\nvoid setup() {}\n");
    let bundled = fixture.select(&HashMap::new(), &[]);

    assert_eq!(bundled.names, vec!["ESP8266WiFi".to_string()]);
    assert!(
        Fixture::file_name_is_source(&bundled.sources, "ESP8266WiFiImpl.cpp"),
        "expected ESP8266WiFiImpl.cpp in {:?}",
        bundled.sources
    );
    assert!(!Fixture::file_name_is_source(
        &bundled.sources,
        "ESP8266WebServerImpl.cpp"
    ));
    assert!(!Fixture::file_name_is_source(
        &bundled.sources,
        "UnusedLibImpl.cpp"
    ));
}

/// A sketch that reaches no bundled library compiles none of them — the
/// pre-#1994 behavior for non-WiFi sketches must not regress into
/// "compile all 40 libraries".
#[test]
fn sketch_without_bundled_includes_selects_nothing() {
    let fixture = Fixture::new("void setup() {}\nvoid loop() {}\n");
    let bundled = fixture.select(&HashMap::new(), &[]);

    assert!(bundled.sources.is_empty(), "{:?}", bundled.sources);
    assert!(bundled.names.is_empty(), "{:?}", bundled.names);
}

/// An include whose guard evaluates false must not pull a library into the
/// compile set — the build compiles without it, so linking its objects would
/// only add dead code (the active-branch contract of the shared resolver).
#[test]
fn include_in_inactive_branch_is_not_selected() {
    let fixture = Fixture::new(
        "#define FB_USE_WIFI 0\n#if FB_USE_WIFI\n#include <ESP8266WiFi.h>\n#endif\nvoid setup() {}\n",
    );
    let bundled = fixture.select(&HashMap::new(), &[]);

    assert!(bundled.sources.is_empty(), "{:?}", bundled.sources);
}

/// `#if 0` around an include is PlatformIO's LDF "this dependency is needed"
/// hint, not dead code — the header scanner counts it (`scanner.rs`, the
/// `#if 0` hint idiom), so the library is compiled. Pinned here so the fix for
/// #1994 is not "improved" into ignoring those declarations.
#[test]
fn if_zero_include_is_a_dependency_declaration() {
    let fixture = Fixture::new("#if 0\n#include <ESP8266WiFi.h>\n#endif\nvoid setup() {}\n");
    let bundled = fixture.select(&HashMap::new(), &[]);

    assert_eq!(bundled.names, vec!["ESP8266WiFi".to_string()]);
}

/// `lib_deps` naming a bundled library is an explicit opt-in, the escape
/// hatch when the include walk cannot infer the dependency.
#[test]
fn declared_lib_deps_is_honored() {
    let fixture = Fixture::new("void setup() {}\n");
    let bundled = fixture.select(&HashMap::new(), &["ESP8266WebServer".to_string()]);

    assert_eq!(bundled.names, vec!["ESP8266WebServer".to_string()]);
    assert!(Fixture::file_name_is_source(
        &bundled.sources,
        "ESP8266WebServerImpl.cpp"
    ));
}

/// A missing framework `libraries/` directory (custom cores, `platform_packages`
/// overrides) must resolve to "nothing to compile", never an error.
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

#[test]
fn log_selection_accepts_a_non_empty_selection() {
    let fixture = Fixture::new("#include <ESP8266WiFi.h>\n");
    log_selection(&fixture.select(&HashMap::new(), &[]));
}

#[test]
fn source_paths_stay_under_the_library_dir() {
    let fixture = Fixture::new("#include <ESP8266WiFi.h>\n");
    let bundled = fixture.select(&HashMap::new(), &[]);
    let lib_dir = &fixture.libraries[0].dir;
    assert!(
        bundled.sources.iter().all(|s| s.starts_with(lib_dir)),
        "{:?} not under {}",
        bundled.sources,
        lib_dir.display()
    );
}

#[test]
fn fixture_paths_are_absolute() {
    // Guards the test itself: a relative seed path would make the walker's
    // results depend on the process CWD.
    let fixture = Fixture::new("#include <ESP8266WiFi.h>\n");
    assert!(Path::new(&fixture.project_dir).is_absolute());
    assert!(fixture.src_dir.is_absolute());
}
