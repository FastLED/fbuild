//! Tests for [`super`].
//!
//! Split out to keep the implementation file under the workspace 1000-LOC
//! limit; `compiler_tests.rs` is the same pattern.

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

fn tempdir() -> TempDir {
    TempDir::new_in(fbuild_paths::temp_subdir("fbuild-library-select-tests")).unwrap()
}

#[test]
fn r01_direct_include_selects_library() {
    let tmp = tempdir();
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

/// FastLED/fbuild#1371, end to end: a macro derived inside a header must
/// not hide the include it guards.
///
/// This is the SAMD shape verbatim. The compiler command line carries
/// `__SAMD21G18A__`; `FL_IS_SAMD21` is derived from it several headers
/// deep, and header-defined macros are not threaded through a BFS walk.
/// Evaluating `#if defined(FL_IS_SAMD21)` as *false* made `<SPI.h>`
/// invisible to selection even though the include is genuinely compiled —
/// the build then failed on a missing header that nothing had requested.
#[test]
fn include_guarded_by_a_header_derived_macro_still_selects_its_library() {
    let tmp = tempdir();
    let project_src = tmp.path().join("project").join("src");
    write(&project_src.join("main.cpp"), "#include <FastLED.h>\n");
    write(
        &project_src.join("FastLED.h"),
        "#include <is_platform.h>\n#include <fastspi_arm_sam.h>\n",
    );
    // The derivation the walk cannot see: command-line macro in, FastLED
    // macro out.
    write(
        &project_src.join("is_platform.h"),
        "#if defined(__SAMD21G18A__)\n#define FL_IS_SAMD21 1\n#endif\n",
    );
    write(
        &project_src.join("fastspi_arm_sam.h"),
        "#if defined(FL_IS_SAMD21) || defined(FL_IS_SAMD51)\n#include <SPI.h>\n#endif\n",
    );

    let mut spi = lib(tmp.path(), "SPI");
    write(&spi.include_dirs[0].join("SPI.h"), "");
    let spi_cpp = spi.include_dirs[0].join("SPI.cpp");
    write(&spi_cpp, "");
    spi.source_files.push(spi_cpp);

    let mut defines = HashMap::new();
    defines.insert("__SAMD21G18A__".to_string(), "1".to_string());

    let seeds = vec![project_src.join("main.cpp")];
    let selection = resolve_active(&seeds, &[project_src], &[spi], &defines);
    assert_eq!(
        selection.required_libraries,
        vec!["SPI".to_string()],
        "an include behind a header-derived guard must still select its library"
    );
}

/// The `#if 0` LDF hint idiom, end to end.
///
/// `platforms/ldf_headers.h` in FastLED declares dependencies inside
/// `#if 0` blocks precisely because PlatformIO's `chain` LDF scans
/// includes without evaluating conditionals. The block never compiles, so
/// an include there exists only to be seen.
#[test]
fn if_zero_hint_headers_declare_a_dependency() {
    let tmp = tempdir();
    let project_src = tmp.path().join("project").join("src");
    write(&project_src.join("main.cpp"), "#include <FastLED.h>\n");
    write(&project_src.join("FastLED.h"), "#include <ldf_headers.h>\n");
    write(
        &project_src.join("ldf_headers.h"),
        "#if 0\n#include <SPI.h>\n#endif\n",
    );

    let mut spi = lib(tmp.path(), "SPI");
    write(&spi.include_dirs[0].join("SPI.h"), "");
    let spi_cpp = spi.include_dirs[0].join("SPI.cpp");
    write(&spi_cpp, "");
    spi.source_files.push(spi_cpp);

    let seeds = vec![project_src.join("main.cpp")];
    let selection = resolve_active(&seeds, &[project_src], &[spi], &HashMap::new());
    assert_eq!(
        selection.required_libraries,
        vec!["SPI".to_string()],
        "an #if 0 hint must declare the dependency"
    );
}

/// A guard on a macro *nothing* defines stays honestly false.
///
/// The counterweight to the two tests above, and the reason this is not
/// simply a textual scan: without it, every unresolved guard would select
/// its library and #1094's over-selection would come straight back.
#[test]
fn guard_on_a_macro_no_file_defines_does_not_select() {
    let tmp = tempdir();
    let project_src = tmp.path().join("project").join("src");
    write(&project_src.join("main.cpp"), "#include <FastLED.h>\n");
    write(
        &project_src.join("FastLED.h"),
        "#if defined(NOBODY_DEFINES_THIS)\n#include <Audio.h>\n#endif\n#include <SPI.h>\n",
    );

    let mut audio = lib(tmp.path(), "Audio");
    write(&audio.include_dirs[0].join("Audio.h"), "");
    let audio_cpp = audio.include_dirs[0].join("Audio.cpp");
    write(&audio_cpp, "");
    audio.source_files.push(audio_cpp);

    let mut spi = lib(tmp.path(), "SPI");
    write(&spi.include_dirs[0].join("SPI.h"), "");
    let spi_cpp = spi.include_dirs[0].join("SPI.cpp");
    write(&spi_cpp, "");
    spi.source_files.push(spi_cpp);

    let seeds = vec![project_src.join("main.cpp")];
    let selection = resolve_active(&seeds, &[project_src], &[audio, spi], &HashMap::new());
    assert_eq!(
        selection.required_libraries,
        vec!["SPI".to_string()],
        "a guard nothing can satisfy must not pull in a library"
    );
}

#[test]
fn active_resolution_skips_library_in_disabled_branch() {
    let tmp = tempdir();
    let project_src = tmp.path().join("project").join("src");
    write(
        &project_src.join("main.cpp"),
        "#define USE_SPI 1\n#include <FastLED.h>\n",
    );
    write(
        &project_src.join("FastLED.h"),
        "#if defined(USE_AUDIO)\n#include <Audio.h>\n#elif USE_SPI\n#include <SPI.h>\n#endif\n",
    );

    let mut audio = lib(tmp.path(), "Audio");
    write(&audio.include_dirs[0].join("Audio.h"), "");
    let audio_cpp = audio.include_dirs[0].join("Audio.cpp");
    write(&audio_cpp, "");
    audio.source_files.push(audio_cpp);

    let mut spi = lib(tmp.path(), "SPI");
    write(&spi.include_dirs[0].join("SPI.h"), "");
    let spi_cpp = spi.include_dirs[0].join("SPI.cpp");
    write(&spi_cpp, "");
    spi.source_files.push(spi_cpp);

    let seeds = vec![project_src.join("main.cpp")];
    let selection = resolve_active(&seeds, &[project_src], &[audio, spi], &HashMap::new());
    assert_eq!(selection.required_libraries, vec!["SPI".to_string()]);
}

#[test]
fn r02_transitive_library_selection() {
    let tmp = tempdir();
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
fn r04_pass2_reconciliation_catches_cpp_only_dependency() {
    // The whole reason the LDF resolver is 2-pass instead of single-pass
    // BFS: a lib's `.cpp` may pull in a second lib that the first lib's
    // `.h` does NOT mention. Pass 1 (BFS from project seeds + reached
    // headers) cannot see that edge; pass 2 re-seeds with each selected
    // lib's full source set and catches it.
    //
    // Setup: project includes <SPI.h>. SPI.h is silent. SPI.cpp includes
    // <Wire.h>. Wire is only reachable through SPI.cpp.
    //
    // Expected: pass 1 selects {SPI}; pass 2 (with SPI.cpp as a seed)
    // selects {SPI, Wire}. A regression that drops the second pass would
    // produce {SPI} only and silently miss Wire at link time.
    let tmp = tempdir();
    let project_src = tmp.path().join("project").join("src");
    write(&project_src.join("main.cpp"), "#include <SPI.h>\n");

    let mut spi = lib(tmp.path(), "SPI");
    write(
        &spi.include_dirs[0].join("SPI.h"),
        "// no transitive includes\n",
    );
    let spi_cpp = spi.include_dirs[0].join("SPI.cpp");
    write(&spi_cpp, "#include <Wire.h>\n");
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
        vec!["SPI".to_string(), "Wire".to_string()],
        "pass 2 reconciliation must catch Wire reached only via SPI.cpp"
    );
}

#[test]
fn r03_no_includes_selects_nothing() {
    let tmp = tempdir();
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
    let tmp = tempdir();
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
    let tmp = tempdir();
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
    let tmp = tempdir();
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
    let tmp = tempdir();
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
    let tmp = tempdir();
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
    let tmp = tempdir();
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

// ---- lib_deps declarations (FastLED/fbuild#1214) ------------------------

/// Build the exact shape from the issue: a sketch that reaches SPI only
/// through a *library* header, which the shallow scan deliberately does
/// not follow. Returns (seeds, search paths, libs, SPI's .cpp).
fn unreachable_spi_fixture(
    tmp: &Path,
) -> (Vec<PathBuf>, Vec<PathBuf>, Vec<FrameworkLibrary>, PathBuf) {
    let project_src = tmp.join("project").join("src");
    write(&project_src.join("main.cpp"), "// no includes at all\n");

    let mut spi = lib(tmp, "SPI");
    write(&spi.include_dirs[0].join("SPI.h"), "");
    let spi_cpp = spi.include_dirs[0].join("SPI.cpp");
    write(&spi_cpp, "");
    spi.source_files.push(spi_cpp.clone());

    (
        vec![project_src.join("main.cpp")],
        vec![project_src],
        vec![spi],
        spi_cpp,
    )
}

/// Baseline: without a declaration the library is NOT selected. This is
/// the shallow-LDF default (#1094) and must stay that way — the fix adds
/// an opt-in, it does not deepen the scan.
#[test]
fn unreached_library_is_not_selected_without_a_declaration() {
    let tmp = tempdir();
    let (seeds, paths, libs, _) = unreachable_spi_fixture(tmp.path());

    let sel = resolve_with_stats_active_declared(&seeds, &paths, &libs, &HashMap::new(), &[]).0;

    assert!(sel.required_libraries.is_empty(), "{sel:?}");
}

#[test]
fn lib_deps_declaration_selects_an_unreached_library() {
    let tmp = tempdir();
    let (seeds, paths, libs, spi_cpp) = unreachable_spi_fixture(tmp.path());

    let sel = resolve_with_stats_active_declared(
        &seeds,
        &paths,
        &libs,
        &HashMap::new(),
        &["SPI".to_string()],
    )
    .0;

    assert_eq!(sel.required_libraries, vec!["SPI".to_string()]);
    assert!(
        sel.source_files.contains(&canon(&spi_cpp)) || sel.source_files.contains(&spi_cpp),
        "declared library's sources must reach the link line: {sel:?}"
    );
}

/// PlatformIO matches `lib_deps` entries case-insensitively and tolerates
/// owner prefixes and version specs. A declaration that doesn't match
/// because of a `@^1.0` suffix would look like the feature is broken.
#[test]
fn lib_deps_matching_ignores_case_owner_and_version() {
    for entry in ["spi", "SPI@^1.0", "arduino/SPI", "arduino/SPI@1.2.3"] {
        let tmp = tempdir();
        let (seeds, paths, libs, _) = unreachable_spi_fixture(tmp.path());

        let sel = resolve_with_stats_active_declared(
            &seeds,
            &paths,
            &libs,
            &HashMap::new(),
            &[entry.to_string()],
        )
        .0;

        assert_eq!(
            sel.required_libraries,
            vec!["SPI".to_string()],
            "entry {entry:?} should match the SPI framework library"
        );
    }
}

/// URLs and local paths name something to be *fetched*; they must not be
/// mangled into a bare name that accidentally matches a framework library.
#[test]
fn lib_deps_urls_and_paths_never_match_a_framework_library() {
    for entry in [
        "https://github.com/example/SPI.git",
        "file:///opt/SPI",
        "./vendor/SPI",
    ] {
        let tmp = tempdir();
        let (seeds, paths, libs, _) = unreachable_spi_fixture(tmp.path());

        let sel = resolve_with_stats_active_declared(
            &seeds,
            &paths,
            &libs,
            &HashMap::new(),
            &[entry.to_string()],
        )
        .0;

        assert!(
            sel.required_libraries.is_empty(),
            "entry {entry:?} must be left to the installer path, got {sel:?}"
        );
    }
}

/// An explicit declaration gets its own dependency chain resolved, exactly
/// as PlatformIO does — the declared library participates in the
/// reconciliation passes rather than being bolted on at the end.
#[test]
fn declared_library_pulls_in_its_own_transitive_dependency() {
    let tmp = tempdir();
    let project_src = tmp.path().join("project").join("src");
    write(&project_src.join("main.cpp"), "// no includes\n");

    let mut wire = lib(tmp.path(), "Wire");
    write(&wire.include_dirs[0].join("Wire.h"), "");
    let wire_cpp = wire.include_dirs[0].join("Wire.cpp");
    write(&wire_cpp, "");
    wire.source_files.push(wire_cpp);

    // SPI.cpp — not its header — is what reaches Wire, so only the
    // reconciliation pass can find it.
    let mut spi = lib(tmp.path(), "SPI");
    write(&spi.include_dirs[0].join("SPI.h"), "");
    let spi_cpp = spi.include_dirs[0].join("SPI.cpp");
    write(&spi_cpp, "#include <Wire.h>\n");
    spi.source_files.push(spi_cpp);

    let seeds = vec![project_src.join("main.cpp")];
    let sel = resolve_with_stats_active_declared(
        &seeds,
        &[project_src],
        &[spi, wire],
        &HashMap::new(),
        &["SPI".to_string()],
    )
    .0;

    assert_eq!(
        sel.required_libraries,
        vec!["SPI".to_string(), "Wire".to_string()],
        "declaring SPI must also bring in the Wire it depends on"
    );
}

#[test]
fn bundled_framework_lib_deps_are_not_external() {
    let tmp = tempdir();
    let libraries = vec![lib(tmp.path(), "BTstackLib"), lib(tmp.path(), "HTTPUpdate")];
    let declared = vec![
        "btSTACKlib".to_string(),
        "vendor/HTTPUpdate@^1.3".to_string(),
        "ExternalRegistryLib@^2.0".to_string(),
        "https://example.com/vendor/local-lib.git".to_string(),
        "https://example.com/vendor/HTTPUpdate".to_string(),
        "file://vendor/BTstackLib".to_string(),
    ];

    assert_eq!(
        external_declared_deps(&declared, &libraries),
        vec![
            "ExternalRegistryLib@^2.0".to_string(),
            "https://example.com/vendor/local-lib.git".to_string(),
            "https://example.com/vendor/HTTPUpdate".to_string(),
            "file://vendor/BTstackLib".to_string(),
        ]
    );
}

#[test]
fn declared_dep_name_normalization() {
    assert_eq!(declared_dep_name("SPI").as_deref(), Some("spi"));
    assert_eq!(declared_dep_name("  Wire@^1.0  ").as_deref(), Some("wire"));
    assert_eq!(
        declared_dep_name("adafruit/Adafruit GFX").as_deref(),
        Some("adafruit gfx")
    );
    assert_eq!(declared_dep_name(""), None);
    assert_eq!(declared_dep_name("https://example.com/x.git"), None);
    assert_eq!(declared_dep_name("./local"), None);
}
