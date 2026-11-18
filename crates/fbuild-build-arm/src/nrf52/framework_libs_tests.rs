//! Tests for nRF52 bundled Arduino-library selection (FastLED/fbuild#2072).
//!
//! The Adafruit nRF52 BSP ships its libraries in the *flat* 1.0 Arduino
//! layout — `libraries/Wire/Wire.h` next to `libraries/Wire/Wire_nRF52.cpp`,
//! no `src/` — so the fixture mirrors that, not the 1.5 layout.

use std::collections::HashMap;

use fbuild_core::path::NormalizedPath;

use super::{log_selection, select_bundled_libraries};

/// Synthetic framework tree shaped like `framework-arduinoadafruitnrf52`:
/// one header plus one uniquely named `.cpp` per library, flat layout.
struct Fixture {
    _dir: tempfile::TempDir,
    libraries_dir: NormalizedPath,
    project_dir: NormalizedPath,
    src_dir: NormalizedPath,
    include_dirs: Vec<NormalizedPath>,
}

impl Fixture {
    fn new(sketch: &str) -> Self {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().to_path_buf();
        let libraries_dir = NormalizedPath::new(root.join("framework").join("libraries"));
        // flat (1.0) layout: header + sources at the library root
        for (name, header, sources) in [
            ("Wire", "Wire.h", &["Wire_nRF52.cpp"][..]),
            ("SPI", "SPI.h", &["SPI.cpp", "SPI_nrf52832.cpp"][..]),
            ("Bluefruit52Lib", "bluefruit.h", &["Bluefruit.cpp"][..]),
            (
                "Adafruit_LittleFS",
                "Adafruit_LittleFS.h",
                &["Adafruit_LittleFS.cpp"][..],
            ),
        ] {
            let lib_dir = libraries_dir.as_path().join(name);
            std::fs::create_dir_all(&lib_dir).unwrap();
            std::fs::write(
                lib_dir.join(header),
                format!("#pragma once\nclass {name}Class {{ public: void begin(); }};\n"),
            )
            .unwrap();
            for source in sources {
                std::fs::write(
                    lib_dir.join(source),
                    format!("#include \"{header}\"\nvoid {name}Class::begin() {{}}\n"),
                )
                .unwrap();
            }
        }
        // TinyUSB is 1.5 layout and compiled by the orchestrator itself.
        let tinyusb_src = libraries_dir
            .as_path()
            .join("Adafruit_TinyUSB_Arduino")
            .join("src");
        std::fs::create_dir_all(&tinyusb_src).unwrap();
        std::fs::write(tinyusb_src.join("Adafruit_TinyUSB.h"), "#pragma once\n").unwrap();
        std::fs::write(tinyusb_src.join("tusb.c"), "int tusb(void) { return 0; }\n").unwrap();

        let project_dir = root.join("project");
        let src_dir = project_dir.join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(src_dir.join("main.cpp"), sketch).unwrap();

        Self {
            _dir: dir,
            libraries_dir,
            project_dir: NormalizedPath::new(&project_dir),
            src_dir: NormalizedPath::new(&src_dir),
            // the compiler's own path: core/variant/SDK, never the libraries/
            // tree — resolution must come from the finder's own scan
            include_dirs: vec![NormalizedPath::new(&src_dir)],
        }
    }

    fn select(
        &self,
        defines: &HashMap<String, String>,
        declared: &[String],
        already_compiled: &[NormalizedPath],
    ) -> super::BundledLibraries {
        select_bundled_libraries(
            self.libraries_dir.as_path(),
            self.project_dir.as_path(),
            self.src_dir.as_path(),
            defines,
            declared,
            &self.include_dirs,
            already_compiled,
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
    bundled
        .include_dirs
        .iter()
        .any(|dir| dir.as_path().file_name().and_then(|f| f.to_str()) == Some(dir_name))
}

/// The reported defect: a sketch that calls `Wire.begin()` must get
/// `Wire_nRF52.cpp` in the compile set, otherwise the link fails with
/// `undefined reference to TwoWire::begin()`.
#[test]
fn sketch_include_compiles_the_bundled_library() {
    let fixture = Fixture::new("#include <Wire.h>\nvoid setup() { Wire.begin(); }\n");
    let bundled = fixture.select(&HashMap::new(), &[], &[]);

    assert_eq!(bundled.names, vec!["Wire".to_string()]);
    assert!(
        has_source(&bundled, "Wire_nRF52.cpp"),
        "expected Wire_nRF52.cpp in {:?}",
        bundled.sources
    );
    assert!(
        has_include_dir(&bundled, "Wire"),
        "expected the Wire library dir in {:?}",
        bundled.include_dirs
    );
    assert!(!has_source(&bundled, "SPI.cpp"));
    log_selection(&bundled);
}

/// An unreached library must contribute nothing — `Bluefruit52Lib` is large
/// and its headers must never shadow a core header.
#[test]
fn unreached_library_contributes_nothing() {
    let fixture = Fixture::new("#include <Wire.h>\nvoid setup() {}\n");
    let bundled = fixture.select(&HashMap::new(), &[], &[]);

    assert!(
        !has_source(&bundled, "Bluefruit.cpp"),
        "{:?}",
        bundled.sources
    );
    assert!(!has_source(&bundled, "Adafruit_LittleFS.cpp"));
    assert!(!has_include_dir(&bundled, "Bluefruit52Lib"));
}

/// Both SPI translation units are flat-layout siblings; the nRF52832 one is
/// `#ifdef NRF52832_XXAA`-guarded, so compiling both is what Arduino does.
#[test]
fn flat_layout_library_contributes_every_source() {
    let fixture = Fixture::new("#include <SPI.h>\nvoid setup() { SPI.begin(); }\n");
    let bundled = fixture.select(&HashMap::new(), &[], &[]);

    assert!(has_source(&bundled, "SPI.cpp"), "{:?}", bundled.sources);
    assert!(has_source(&bundled, "SPI_nrf52832.cpp"));
}

/// TinyUSB is compiled by the orchestrator's dedicated block; selecting it
/// again would duplicate translation units.
#[test]
fn orchestrator_compiled_library_is_never_selected() {
    let fixture = Fixture::new("#include <Adafruit_TinyUSB.h>\nvoid setup() {}\n");
    let bundled = fixture.select(&HashMap::new(), &[], &[]);

    assert!(!has_source(&bundled, "tusb.c"), "{:?}", bundled.sources);
    assert!(
        !bundled
            .names
            .contains(&"Adafruit_TinyUSB_Arduino".to_string())
    );
}

/// The orchestrator already pushes `SPI.cpp` for the TinyUSB host adapter;
/// the selector must not hand it back a second time.
#[test]
fn already_compiled_sources_are_not_duplicated() {
    let fixture = Fixture::new("#include <SPI.h>\nvoid setup() { SPI.begin(); }\n");
    let manual = vec![NormalizedPath::new(
        fixture.libraries_dir.as_path().join("SPI").join("SPI.cpp"),
    )];
    let bundled = fixture.select(&HashMap::new(), &[], &manual);

    assert!(!has_source(&bundled, "SPI.cpp"), "{:?}", bundled.sources);
    assert!(has_source(&bundled, "SPI_nrf52832.cpp"));
}

/// `lib_deps` is an explicit project declaration and selects a library the
/// shallow scan cannot infer.
#[test]
fn declared_lib_deps_selects_a_library() {
    let fixture = Fixture::new("void setup() {}\n");
    let bundled = fixture.select(&HashMap::new(), &["Bluefruit52Lib".to_string()], &[]);

    assert_eq!(bundled.names, vec!["Bluefruit52Lib".to_string()]);
    assert!(
        has_source(&bundled, "Bluefruit.cpp"),
        "{:?}",
        bundled.sources
    );
}

/// A core without a `libraries/` tree is not an error.
#[test]
fn missing_libraries_dir_is_empty() {
    let fixture = Fixture::new("#include <Wire.h>\nvoid setup() {}\n");
    let missing = fixture.libraries_dir.join("nope");
    let bundled = select_bundled_libraries(
        missing.as_path(),
        fixture.project_dir.as_path(),
        fixture.src_dir.as_path(),
        &HashMap::new(),
        &[],
        &fixture.include_dirs,
        &[],
    )
    .unwrap();
    assert!(bundled.sources.is_empty());
    assert!(bundled.include_dirs.is_empty());
}
