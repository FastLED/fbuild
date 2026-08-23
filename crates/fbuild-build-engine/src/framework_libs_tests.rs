//! Tests for [`super`] — framework-library selection.
//!
//! Split out to keep `framework_libs.rs` under the workspace 1000-LOC
//! limit; `compiler_tests.rs` is the same pattern.

use super::*;

#[test]
fn resolves_libraries_from_project_includes() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project_src = tmp.path().join("project").join("src");
    std::fs::create_dir_all(&project_src).unwrap();
    std::fs::write(
        project_src.join("main.cpp"),
        "#include <SPI.h>\n#include <OctoWS2811.h>\n",
    )
    .unwrap();

    let spi_dir = tmp.path().join("framework").join("libraries").join("SPI");
    std::fs::create_dir_all(&spi_dir).unwrap();
    std::fs::write(spi_dir.join("SPI.h"), "").unwrap();
    std::fs::write(spi_dir.join("SPI.cpp"), "").unwrap();

    let octo_dir = tmp
        .path()
        .join("framework")
        .join("libraries")
        .join("OctoWS2811");
    std::fs::create_dir_all(&octo_dir).unwrap();
    std::fs::write(octo_dir.join("OctoWS2811.h"), "").unwrap();
    std::fs::write(octo_dir.join("OctoWS2811.cpp"), "").unwrap();
    std::fs::write(octo_dir.join("OctoWS2811_imxrt.cpp"), "").unwrap();

    let libraries = vec![
        FrameworkLibrary {
            name: "OctoWS2811".to_string(),
            dir: octo_dir.clone(),
            include_dirs: vec![octo_dir.clone()],
            source_files: vec![
                octo_dir.join("OctoWS2811.cpp"),
                octo_dir.join("OctoWS2811_imxrt.cpp"),
            ],
        },
        FrameworkLibrary {
            name: "SPI".to_string(),
            dir: spi_dir.clone(),
            include_dirs: vec![spi_dir.clone()],
            source_files: vec![spi_dir.join("SPI.cpp")],
        },
    ];

    let mut sources = resolve_framework_library_sources_from_libraries(
        &libraries,
        std::slice::from_ref(&project_src),
    );
    sources.sort();

    let mut expected = vec![
        octo_dir.join("OctoWS2811.cpp"),
        octo_dir.join("OctoWS2811_imxrt.cpp"),
        spi_dir.join("SPI.cpp"),
    ];
    expected.sort();
    assert_eq!(sources, expected);
}

#[test]
fn follows_transitive_includes() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project_src = tmp.path().join("project").join("src");
    std::fs::create_dir_all(&project_src).unwrap();
    std::fs::write(project_src.join("main.cpp"), "#include <NeedsSpi.h>\n").unwrap();

    let spi_dir = tmp.path().join("framework").join("libraries").join("SPI");
    std::fs::create_dir_all(&spi_dir).unwrap();
    std::fs::write(spi_dir.join("SPI.h"), "").unwrap();
    std::fs::write(spi_dir.join("SPI.cpp"), "").unwrap();

    let wrapper_dir = tmp
        .path()
        .join("framework")
        .join("libraries")
        .join("NeedsSpi");
    std::fs::create_dir_all(&wrapper_dir).unwrap();
    std::fs::write(wrapper_dir.join("NeedsSpi.h"), "#include <SPI.h>\n").unwrap();
    std::fs::write(wrapper_dir.join("NeedsSpi.cpp"), "").unwrap();

    let libraries = vec![
        FrameworkLibrary {
            name: "NeedsSpi".to_string(),
            dir: wrapper_dir.clone(),
            include_dirs: vec![wrapper_dir.clone()],
            source_files: vec![wrapper_dir.join("NeedsSpi.cpp")],
        },
        FrameworkLibrary {
            name: "SPI".to_string(),
            dir: spi_dir.clone(),
            include_dirs: vec![spi_dir.clone()],
            source_files: vec![spi_dir.join("SPI.cpp")],
        },
    ];

    let mut sources = resolve_framework_library_sources_from_libraries(
        &libraries,
        std::slice::from_ref(&project_src),
    );
    sources.sort();

    let mut expected = vec![wrapper_dir.join("NeedsSpi.cpp"), spi_dir.join("SPI.cpp")];
    expected.sort();
    assert_eq!(sources, expected);
}

#[test]
fn unrelated_library_not_selected() {
    // Regression guard for #204: libraries whose headers are never
    // referenced must not appear in the compile set.
    let tmp = tempfile::TempDir::new().unwrap();
    let project_src = tmp.path().join("project").join("src");
    std::fs::create_dir_all(&project_src).unwrap();
    std::fs::write(project_src.join("main.cpp"), "#include <SPI.h>\n").unwrap();

    let spi_dir = tmp.path().join("framework").join("libraries").join("SPI");
    std::fs::create_dir_all(&spi_dir).unwrap();
    std::fs::write(spi_dir.join("SPI.h"), "").unwrap();
    std::fs::write(spi_dir.join("SPI.cpp"), "").unwrap();

    let fnet_dir = tmp.path().join("framework").join("libraries").join("FNET");
    std::fs::create_dir_all(&fnet_dir).unwrap();
    std::fs::write(fnet_dir.join("fnet.h"), "").unwrap();
    std::fs::write(fnet_dir.join("fnet.cpp"), "").unwrap();

    let libraries = vec![
        FrameworkLibrary {
            name: "FNET".to_string(),
            dir: fnet_dir.clone(),
            include_dirs: vec![fnet_dir.clone()],
            source_files: vec![fnet_dir.join("fnet.cpp")],
        },
        FrameworkLibrary {
            name: "SPI".to_string(),
            dir: spi_dir.clone(),
            include_dirs: vec![spi_dir.clone()],
            source_files: vec![spi_dir.join("SPI.cpp")],
        },
    ];

    let sources = resolve_framework_library_sources_from_libraries(
        &libraries,
        std::slice::from_ref(&project_src),
    );
    assert_eq!(sources, vec![spi_dir.join("SPI.cpp")]);
}

/// FastLED/fbuild#1337: a local library's own *source* must be able to
/// select a framework library.
///
/// The Teensy shape. FastLED lives under `lib/FastLED/`, and the include
/// that needs `Adafruit_NeoPixel` sits in one of its `.cpp` translation
/// units — not in any header the sketch reaches. Seeding only the sketch
/// meant the include was never scanned, so the library compiled against a
/// header on the include path whose sources were never on the link line:
/// `undefined reference to Adafruit_NeoPixel::*`, on all eight Teensy
/// boards.
///
/// The guard is FastLED's current one — an explicit opt-in `-D` *and* a
/// header probe — so this also pins the two-signal behavior.
#[test]
fn local_library_source_selects_a_framework_library() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project = tmp.path().join("project");
    let project_src = project.join("src");
    let fastled_src = project.join("lib").join("FastLED").join("src");
    std::fs::create_dir_all(&project_src).unwrap();
    std::fs::create_dir_all(&fastled_src).unwrap();

    std::fs::write(project_src.join("main.cpp"), "#include <FastLED.h>\n").unwrap();
    std::fs::write(fastled_src.join("FastLED.h"), "// no adafruit here\n").unwrap();
    // The compiled TU of the local library, and the only place the
    // dependency is expressed.
    std::fs::write(
        fastled_src.join("adafruit_driver.cpp"),
        "#include \"has_include.h\"\n\
         #if defined(FASTLED_USE_ADAFRUIT_NEOPIXEL) && FL_HAS_INCLUDE(<Adafruit_NeoPixel.h>)\n\
         #include <Adafruit_NeoPixel.h>\n\
         #endif\n",
    )
    .unwrap();
    std::fs::write(
        fastled_src.join("has_include.h"),
        "#ifndef FL_HAS_INCLUDE_H\n#define FL_HAS_INCLUDE_H\n\
         #define FL_HAS_INCLUDE(x) __has_include(x)\n#endif\n",
    )
    .unwrap();

    let neopixel = tmp
        .path()
        .join("framework")
        .join("libraries")
        .join("Adafruit_NeoPixel");
    std::fs::create_dir_all(&neopixel).unwrap();
    std::fs::write(neopixel.join("Adafruit_NeoPixel.h"), "").unwrap();
    std::fs::write(neopixel.join("Adafruit_NeoPixel.cpp"), "").unwrap();

    let libraries = vec![FrameworkLibrary {
        name: "Adafruit_NeoPixel".to_string(),
        dir: neopixel.clone(),
        include_dirs: vec![neopixel.clone()],
        source_files: vec![neopixel.join("Adafruit_NeoPixel.cpp")],
    }];

    let mut defines = HashMap::new();
    defines.insert("FASTLED_USE_ADAFRUIT_NEOPIXEL".to_string(), "1".to_string());

    let sources =
        resolve_framework_library_sources_active(&libraries, &project, &project_src, &defines);
    assert!(
        sources.iter().any(|p| p.ends_with("Adafruit_NeoPixel.cpp")),
        "a compiled library source's include must reach the link line: {sources:?}"
    );
}

/// The other direction of the same agreement: no opt-in, no link.
///
/// FastLED's guard is `defined(FASTLED_USE_ADAFRUIT_NEOPIXEL) &&
/// FL_HAS_INCLUDE(...)`, deliberately two signals. Nobody `#define`s the
/// opt-in, so it is honestly false and the real driver is not compiled —
/// selecting the library here would put sources on the link line that
/// nothing references. Scanner and compiler have to agree both ways.
#[test]
fn local_library_source_without_the_opt_in_selects_nothing() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project = tmp.path().join("project");
    let project_src = project.join("src");
    let fastled_src = project.join("lib").join("FastLED").join("src");
    std::fs::create_dir_all(&project_src).unwrap();
    std::fs::create_dir_all(&fastled_src).unwrap();

    std::fs::write(project_src.join("main.cpp"), "#include <FastLED.h>\n").unwrap();
    std::fs::write(fastled_src.join("FastLED.h"), "// no adafruit here\n").unwrap();
    std::fs::write(
        fastled_src.join("adafruit_driver.cpp"),
        "#include \"has_include.h\"\n\
         #if defined(FASTLED_USE_ADAFRUIT_NEOPIXEL) && FL_HAS_INCLUDE(<Adafruit_NeoPixel.h>)\n\
         #include <Adafruit_NeoPixel.h>\n\
         #endif\n",
    )
    .unwrap();
    std::fs::write(
        fastled_src.join("has_include.h"),
        "#ifndef FL_HAS_INCLUDE_H\n#define FL_HAS_INCLUDE_H\n\
         #define FL_HAS_INCLUDE(x) __has_include(x)\n#endif\n",
    )
    .unwrap();

    let neopixel = tmp
        .path()
        .join("framework")
        .join("libraries")
        .join("Adafruit_NeoPixel");
    std::fs::create_dir_all(&neopixel).unwrap();
    std::fs::write(neopixel.join("Adafruit_NeoPixel.h"), "").unwrap();
    std::fs::write(neopixel.join("Adafruit_NeoPixel.cpp"), "").unwrap();

    let libraries = vec![FrameworkLibrary {
        name: "Adafruit_NeoPixel".to_string(),
        dir: neopixel.clone(),
        include_dirs: vec![neopixel.clone()],
        source_files: vec![neopixel.join("Adafruit_NeoPixel.cpp")],
    }];

    // No `FASTLED_USE_ADAFRUIT_NEOPIXEL` in the defines.
    let sources = resolve_framework_library_sources_active(
        &libraries,
        &project,
        &project_src,
        &HashMap::new(),
    );
    assert!(
        sources.is_empty(),
        "an un-opted-in driver must not put a library on the link line: {sources:?}"
    );
}

/// A library's `examples/` are shipped but never compiled, so they must
/// never seed.
///
/// This is the mirror-image failure of #1337: seeding an example sketch
/// would make the scanner claim a dependency the build does not link,
/// which is the same scanner/compiler disagreement pointed the other way.
#[test]
fn local_library_examples_do_not_seed() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project = tmp.path().join("project");
    let project_src = project.join("src");
    let fastled = project.join("lib").join("FastLED");
    std::fs::create_dir_all(&project_src).unwrap();
    std::fs::create_dir_all(fastled.join("src")).unwrap();
    std::fs::create_dir_all(fastled.join("examples").join("Demo")).unwrap();

    std::fs::write(project_src.join("main.cpp"), "#include <FastLED.h>\n").unwrap();
    std::fs::write(fastled.join("src").join("FastLED.h"), "").unwrap();
    std::fs::write(fastled.join("src").join("FastLED.cpp"), "").unwrap();
    // Compiled by nobody, and it names a library the build must not link.
    std::fs::write(
        fastled.join("examples").join("Demo").join("Demo.ino"),
        "#include <Audio.h>\n",
    )
    .unwrap();

    let audio = tmp.path().join("framework").join("libraries").join("Audio");
    std::fs::create_dir_all(&audio).unwrap();
    std::fs::write(audio.join("Audio.h"), "").unwrap();
    std::fs::write(audio.join("Audio.cpp"), "").unwrap();

    let libraries = vec![FrameworkLibrary {
        name: "Audio".to_string(),
        dir: audio.clone(),
        include_dirs: vec![audio.clone()],
        source_files: vec![audio.join("Audio.cpp")],
    }];

    let sources = resolve_framework_library_sources_active(
        &libraries,
        &project,
        &project_src,
        &HashMap::new(),
    );
    assert!(
        sources.is_empty(),
        "an example sketch is not compiled, so it must not select: {sources:?}"
    );
}

#[test]
fn inactive_local_library_header_cannot_select_framework_library() {
    // FastLED/fbuild#1094: a header anywhere under project lib/ used to
    // become an independent seed. Its inactive include then selected a
    // framework library even though the sketch could not reach it.
    let tmp = tempfile::TempDir::new().unwrap();
    let project_src = tmp.path().join("project").join("src");
    let project_lib = tmp.path().join("project").join("lib");
    let fastled = project_lib.join("FastLED");
    std::fs::create_dir_all(&project_src).unwrap();
    std::fs::create_dir_all(&fastled).unwrap();
    std::fs::write(project_src.join("main.cpp"), "#include <FastLED.h>\n").unwrap();
    std::fs::write(fastled.join("FastLED.h"), "#include <SPI.h>\n").unwrap();
    std::fs::write(fastled.join("inactive_audio.h"), "#include <Audio.h>\n").unwrap();

    let spi_dir = tmp.path().join("framework").join("libraries").join("SPI");
    std::fs::create_dir_all(&spi_dir).unwrap();
    std::fs::write(spi_dir.join("SPI.h"), "").unwrap();
    std::fs::write(spi_dir.join("SPI.cpp"), "").unwrap();

    let audio_dir = tmp.path().join("framework").join("libraries").join("Audio");
    std::fs::create_dir_all(&audio_dir).unwrap();
    std::fs::write(audio_dir.join("Audio.h"), "").unwrap();
    std::fs::write(audio_dir.join("Audio.cpp"), "").unwrap();

    let libraries = vec![
        FrameworkLibrary {
            name: "Audio".to_string(),
            dir: audio_dir.clone(),
            include_dirs: vec![audio_dir.clone()],
            source_files: vec![audio_dir.join("Audio.cpp")],
        },
        FrameworkLibrary {
            name: "SPI".to_string(),
            dir: spi_dir.clone(),
            include_dirs: vec![spi_dir.clone()],
            source_files: vec![spi_dir.join("SPI.cpp")],
        },
    ];

    let sources =
        resolve_framework_library_sources_from_libraries(&libraries, &[project_src, project_lib]);
    assert_eq!(sources, vec![spi_dir.join("SPI.cpp")]);
}

#[test]
fn prefers_local_library_over_framework() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project_src = tmp.path().join("project").join("src");
    let project_lib = tmp
        .path()
        .join("project")
        .join("lib")
        .join("FastLED")
        .join("src");
    std::fs::create_dir_all(&project_src).unwrap();
    std::fs::create_dir_all(&project_lib).unwrap();
    std::fs::write(project_src.join("main.cpp"), "#include <FastLED.h>\n").unwrap();
    std::fs::write(project_lib.join("FastLED.h"), "#include <SPI.h>\n").unwrap();
    std::fs::write(project_lib.join("FastLED.cpp"), "").unwrap();

    let framework_fastled_dir = tmp
        .path()
        .join("framework")
        .join("libraries")
        .join("FastLED");
    std::fs::create_dir_all(&framework_fastled_dir).unwrap();
    std::fs::write(framework_fastled_dir.join("FastLED.h"), "").unwrap();
    std::fs::write(framework_fastled_dir.join("FastLED.cpp"), "").unwrap();

    let spi_dir = tmp.path().join("framework").join("libraries").join("SPI");
    std::fs::create_dir_all(&spi_dir).unwrap();
    std::fs::write(spi_dir.join("SPI.h"), "").unwrap();
    std::fs::write(spi_dir.join("SPI.cpp"), "").unwrap();

    let libraries = vec![
        FrameworkLibrary {
            name: "FastLED".to_string(),
            dir: framework_fastled_dir.clone(),
            include_dirs: vec![framework_fastled_dir.clone()],
            source_files: vec![framework_fastled_dir.join("FastLED.cpp")],
        },
        FrameworkLibrary {
            name: "SPI".to_string(),
            dir: spi_dir.clone(),
            include_dirs: vec![spi_dir.clone()],
            source_files: vec![spi_dir.join("SPI.cpp")],
        },
    ];

    let roots = vec![project_src, project_lib];
    let sources = resolve_framework_library_sources_from_libraries(&libraries, &roots);

    assert_eq!(sources, vec![spi_dir.join("SPI.cpp")]);
}

/// Regression for FastLED/fbuild#263 — case A: when the user's project
/// IS the library (FastLED's own source tree has `src/FastLED.h`
/// directly under one of the walker's roots), the framework's bundled
/// FastLED at `cores/teensy4/libraries/FastLED/` must not get selected.
/// This case works in the LDF resolver today because path-prefix
/// attribution finds `project/src/FastLED.h` first.
#[test]
fn project_is_the_library_does_not_pull_in_bundled_copy() {
    let tmp = tempfile::TempDir::new().unwrap();

    let project_src = tmp.path().join("project").join("src");
    std::fs::create_dir_all(&project_src).unwrap();
    std::fs::write(project_src.join("FastLED.h"), "// the real FastLED\n").unwrap();
    std::fs::write(project_src.join("FastLED.cpp"), "// user impl\n").unwrap();
    std::fs::write(
        project_src.join("example_main.cpp"),
        "#include <FastLED.h>\n",
    )
    .unwrap();

    let bundled_fastled_dir = tmp
        .path()
        .join("framework")
        .join("libraries")
        .join("FastLED");
    std::fs::create_dir_all(&bundled_fastled_dir).unwrap();
    std::fs::write(
        bundled_fastled_dir.join("FastLED.h"),
        "// bundled (stale) FastLED\n",
    )
    .unwrap();
    std::fs::write(bundled_fastled_dir.join("FastLED.cpp"), "// bundled impl\n").unwrap();

    let libraries = vec![FrameworkLibrary {
        name: "FastLED".to_string(),
        dir: bundled_fastled_dir.clone(),
        include_dirs: vec![bundled_fastled_dir.clone()],
        source_files: vec![bundled_fastled_dir.join("FastLED.cpp")],
    }];

    let sources = resolve_framework_library_sources_from_libraries(
        &libraries,
        std::slice::from_ref(&project_src),
    );

    assert!(
        sources.is_empty(),
        "bundled FastLED must NOT be selected when the project owns FastLED.h \
         directly under src/ — see #263. Got: {sources:?}"
    );
}

/// Regression for FastLED/fbuild#263 — case B: the user's project owns
/// FastLED.h at a path that is NOT one of the walker roots passed to
/// the resolver (e.g. `<repo>/src/FastLED.h` while the resolver only
/// sees `<repo>/tests/platform/teensy41/src/`). The walker then can
/// only find FastLED.h via the framework's bundled
/// `cores/teensy4/libraries/FastLED/` include dir, mis-attributes the
/// include to the bundled library, and pulls its sources into the
/// build set — duplicate-symbol time. The fix in `framework_libs.rs`
/// drops framework libraries whose primary header is shadowed by a
/// project header even when the project header isn't first in the
/// search order.
#[test]
fn example_only_root_does_not_pull_in_bundled_fastled_when_user_owns_fastled() {
    let tmp = tempfile::TempDir::new().unwrap();

    // The repo: user's local FastLED lives at <repo>/src/, which is
    // NOT among the resolver's roots for the per-example build.
    let repo_src = tmp.path().join("repo").join("src");
    std::fs::create_dir_all(&repo_src).unwrap();
    std::fs::write(repo_src.join("FastLED.h"), "// the real FastLED\n").unwrap();
    std::fs::write(repo_src.join("FastLED.cpp"), "// user impl\n").unwrap();

    // The per-example project root the resolver actually sees.
    let example_src = tmp
        .path()
        .join("repo")
        .join("tests")
        .join("platform")
        .join("teensy41")
        .join("src");
    std::fs::create_dir_all(&example_src).unwrap();
    std::fs::write(
        example_src.join("example_main.cpp"),
        "#include <FastLED.h>\n",
    )
    .unwrap();

    // Framework bundles its own FastLED.
    let bundled_fastled_dir = tmp
        .path()
        .join("framework")
        .join("libraries")
        .join("FastLED");
    std::fs::create_dir_all(&bundled_fastled_dir).unwrap();
    std::fs::write(bundled_fastled_dir.join("FastLED.h"), "// bundled\n").unwrap();
    std::fs::write(bundled_fastled_dir.join("FastLED.cpp"), "// bundled impl\n").unwrap();

    let libraries = vec![FrameworkLibrary {
        name: "FastLED".to_string(),
        dir: bundled_fastled_dir.clone(),
        include_dirs: vec![bundled_fastled_dir.clone()],
        source_files: vec![bundled_fastled_dir.join("FastLED.cpp")],
    }];

    // The fbuild build pipeline calls `local_overridden_framework_libs`
    // with both the example root AND the repo's actual src/ as
    // shadowing roots. The repo src/FastLED.h shadows the framework's
    // FastLED → framework library is filtered out before the resolver
    // ever sees it.
    let shadowing_roots = vec![example_src.clone(), repo_src.clone()];
    let filtered = filter_framework_libs_shadowed_by_project(&libraries, &shadowing_roots);

    // Resolver runs on the FILTERED library set.
    let sources = resolve_framework_library_sources_from_libraries(
        &filtered,
        std::slice::from_ref(&example_src),
    );

    assert!(
        sources.is_empty(),
        "bundled FastLED must be filtered out because the user's repo owns \
         FastLED.h even when it's not in the per-example walker roots — see #263. \
         Got: {sources:?}"
    );
}

/// Regression for FastLED/fbuild#284 — a nested project header whose
/// basename happens to collide with a framework library's primary
/// header must NOT trigger the shadowing filter. FastLED ships
/// `lib/FastLED/fl/channels/spi.h`, which is includeable only as
/// `<fl/channels/spi.h>`, never as `<SPI.h>`. The framework's `SPI`
/// library must therefore stay in the build set, otherwise every
/// Teensy 4.x example fails at link with `undefined reference to
/// SPIClass::*`.
///
/// At the same time, the existing `#263` behaviour for headers
/// reachable as bare `<basename>` (e.g. `lib/FastLED/noise.h` or
/// `project/src/FastLED.h`) must still drop the matching framework
/// library.
#[test]
fn nested_basename_does_not_shadow_framework_library() {
    let tmp = tempfile::TempDir::new().unwrap();

    // PIO project layout: lib/FastLED/ contains FastLED's source
    // tree directly (1.0 flat layout — no src/ subdir). spi.h is
    // nested deep, noise.h sits at FastLED's include root.
    let project_dir = tmp.path().join("project");
    let lib_dir = project_dir.join("lib");
    let fastled_dir = lib_dir.join("FastLED");
    let nested_spi_dir = fastled_dir.join("fl").join("channels");
    std::fs::create_dir_all(&nested_spi_dir).unwrap();
    std::fs::write(nested_spi_dir.join("spi.h"), "// FastLED internal\n").unwrap();
    std::fs::write(fastled_dir.join("FastLED.h"), "").unwrap();
    std::fs::write(fastled_dir.join("noise.h"), "// shadows framework Noise\n").unwrap();

    let src_dir = project_dir.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();

    // Framework libs: SPI (must SURVIVE the filter) and Noise (must
    // be dropped because the project owns noise.h at the FastLED
    // library include root).
    let spi_dir = tmp.path().join("framework").join("libraries").join("SPI");
    std::fs::create_dir_all(&spi_dir).unwrap();
    std::fs::write(spi_dir.join("SPI.h"), "").unwrap();
    std::fs::write(spi_dir.join("SPI.cpp"), "").unwrap();

    let noise_dir = tmp.path().join("framework").join("libraries").join("Noise");
    std::fs::create_dir_all(&noise_dir).unwrap();
    std::fs::write(noise_dir.join("noise.h"), "").unwrap();
    std::fs::write(noise_dir.join("noise.cpp"), "").unwrap();

    let libraries = vec![
        FrameworkLibrary {
            name: "Noise".to_string(),
            dir: noise_dir.clone(),
            include_dirs: vec![noise_dir.clone()],
            source_files: vec![noise_dir.join("noise.cpp")],
        },
        FrameworkLibrary {
            name: "SPI".to_string(),
            dir: spi_dir.clone(),
            include_dirs: vec![spi_dir.clone()],
            source_files: vec![spi_dir.join("SPI.cpp")],
        },
    ];

    let shadowing_roots = framework_include_scan_roots(&project_dir, &src_dir);
    let filtered = filter_framework_libs_shadowed_by_project(&libraries, &shadowing_roots);

    let surviving: Vec<&str> = filtered.iter().map(|l| l.name.as_str()).collect();
    assert!(
        surviving.contains(&"SPI"),
        "framework SPI must SURVIVE — nested fl/channels/spi.h is not reachable \
         as <SPI.h> and must not trigger the shadowing filter — see #284. \
         Surviving libraries: {surviving:?}"
    );
    assert!(
        !surviving.contains(&"Noise"),
        "framework Noise must be dropped — lib/FastLED/noise.h sits at the \
         FastLED library include root and is reachable as <noise.h> — see #263. \
         Surviving libraries: {surviving:?}"
    );
}

#[test]
fn cached_resolution_round_trips_through_file_store() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project_dir = tmp.path().join("project");
    let src_dir = project_dir.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(src_dir.join("main.cpp"), "#include <SPI.h>\n").unwrap();

    let spi_dir = tmp.path().join("framework").join("libraries").join("SPI");
    std::fs::create_dir_all(&spi_dir).unwrap();
    std::fs::write(spi_dir.join("SPI.h"), "").unwrap();
    std::fs::write(spi_dir.join("SPI.cpp"), "").unwrap();

    let libraries = vec![FrameworkLibrary {
        name: "SPI".to_string(),
        dir: spi_dir.clone(),
        include_dirs: vec![spi_dir.clone()],
        source_files: vec![spi_dir.join("SPI.cpp")],
    }];

    let framework_root = tmp.path().join("framework");
    let defines = HashMap::new();
    let key_inputs = CacheKeyInputs {
        toolchain_triple: "test-arm-none-eabi",
        framework_install_path: &framework_root,
        framework_version: "0.0.0-test",
        preprocessor_defines: &defines,
        declared_deps: &[],
    };

    let kv = FileKvStore::open(tmp.path().join("kv")).unwrap();

    let (first, hit_first) = resolve_framework_library_sources_cached_with_hit(
        &libraries,
        &project_dir,
        &src_dir,
        &key_inputs,
        &kv,
    );
    assert!(!hit_first, "first call must miss the cache");
    assert_eq!(first, vec![spi_dir.join("SPI.cpp")]);

    let (second, hit_second) = resolve_framework_library_sources_cached_with_hit(
        &libraries,
        &project_dir,
        &src_dir,
        &key_inputs,
        &kv,
    );
    assert!(hit_second, "second call must hit the cache");
    assert_eq!(first, second, "cache hit must yield identical sources");
}
