//! Integration test: `fbuild build` → `fbuild bloat` end-to-end
//! against the latest FastLED master on ESP32-S3.
//!
//! This is the Phase 2 + Phase 3 acceptance gate from the
//! [`fbuild bloat` meta](https://github.com/FastLED/fbuild/issues/434).
//! Exercises the realistic case (latest FastLED, biggest binary, most
//! complex symbol graph) and pins the **shape** of the report —
//! `>2000 symbols`, `>100 map-derived` rows — rather than exact byte
//! counts so it doesn't go red the moment FastLED merges a
//! size-affecting refactor.
//!
//! Network-dependent (fetches FastLED master + the esp-idf toolchain),
//! so guarded by `#[ignore]`. Run via:
//!
//! ```bash
//! soldr cargo test -p fbuild-cli --test bloat_esp32s3_fastled_master \
//!     -- --ignored --nocapture
//! ```
//!
//! Or wire into the nightly CI matrix via `workflow_dispatch` +
//! scheduled weekly so FastLED master regressions surface here too.
//!
//! See:
//! - #434 — `fbuild bloat` meta (this test is the acceptance gate).
//! - #428 — `BuildInfo` toolchain paths.
//! - #438 — `bloat` rename.
//! - #439 — default output dir + path printing.
//! - #441 — default-on bloat report after build.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Build the path to the fbuild CLI binary the test should exercise.
/// `cargo test` sets `CARGO_BIN_EXE_<name>` for every `[[bin]]` in the
/// host crate (`fbuild-cli`'s bin is named `fbuild`); the fallback is
/// the target/debug build path so the test still works under raw
/// `cargo run --test`.
fn fbuild_binary() -> PathBuf {
    if let Some(p) = option_env!("CARGO_BIN_EXE_fbuild") {
        return PathBuf::from(p);
    }
    let exe = if cfg!(windows) {
        "fbuild.exe"
    } else {
        "fbuild"
    };
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("target")
        .join("debug")
        .join(exe)
}

fn run_fbuild(args: &[&str]) -> std::io::Result<std::process::Output> {
    Command::new(fbuild_binary()).args(args).output()
}

/// Scaffold a minimal fbuild project skeleton for ESP32-S3 + FastLED
/// master into `dir`. Writes `platformio.ini` (board=esp32-s3-devkitc-1,
/// `lib_deps = https://github.com/FastLED/FastLED#master`) and a
/// `src/main.cpp` that drives a single WS2812 strand.
fn scaffold_esp32s3_blink_fastled_master(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();

    let platformio_ini = r#"[env:esp32s3]
platform = espressif32
board = esp32-s3-devkitc-1
framework = arduino
lib_deps =
    https://github.com/FastLED/FastLED#master
build_flags = -DBOARD_HAS_PSRAM
"#;
    std::fs::write(dir.join("platformio.ini"), platformio_ini).unwrap();

    let main_cpp = r#"#include <Arduino.h>
#include <FastLED.h>

#define NUM_LEDS 64
#define DATA_PIN 8
CRGB leds[NUM_LEDS];

void setup() {
    FastLED.addLeds<WS2812, DATA_PIN, GRB>(leds, NUM_LEDS);
}

void loop() {
    for (int i = 0; i < NUM_LEDS; i++) {
        leds[i] = CRGB::Blue;
    }
    FastLED.show();
    delay(1000);
    for (int i = 0; i < NUM_LEDS; i++) {
        leds[i] = CRGB::Black;
    }
    FastLED.show();
    delay(1000);
}
"#;
    std::fs::write(dir.join("src").join("main.cpp"), main_cpp).unwrap();
}

/// Read the inner `BuildInfo` JSON for a given env from a project's
/// `build_info_<env>.json`.
fn read_build_info(project: &Path, env: &str) -> serde_json::Value {
    let path = project.join(format!("build_info_{env}.json"));
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let outer: serde_json::Value = serde_json::from_slice(&bytes).expect("build_info.json parses");
    let obj = outer.as_object().expect("outer object");
    assert_eq!(obj.len(), 1, "outer dict has exactly one env key");
    obj.get(env)
        .unwrap_or_else(|| panic!("env {env} present in build_info.json"))
        .clone()
}

/// The end-to-end acceptance test pinned by the #434 meta.
///
/// Steps:
/// 1. Scaffold a fbuild project skeleton in a temp dir.
/// 2. `fbuild build .` (uses fbuild's native esp32 orchestrator).
/// 3. Verify `build_info.json` has the four toolchain paths (#428).
/// 4. `fbuild bloat .` (no flags, no `--nm`).
/// 5. Verify report files exist at the documented path.
/// 6. Verify stdout printed both absolute paths.
/// 7. Verify content invariants (totals, symbol count, map-derived count).
/// 8. Verify markdown table renders a top-flash header.
#[test]
#[ignore = "network: fetches FastLED master + downloads esp-idf toolchain"]
fn bloat_esp32s3_blink_fastled_master_end_to_end() {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let project = tmp.path();
    scaffold_esp32s3_blink_fastled_master(project);

    // Step 2: build.
    let build_out = run_fbuild(&["build", project.to_str().unwrap()]).expect("fbuild build runs");
    assert!(
        build_out.status.success(),
        "fbuild build failed: stdout={}\nstderr={}",
        String::from_utf8_lossy(&build_out.stdout),
        String::from_utf8_lossy(&build_out.stderr),
    );

    // Step 3: build_info.json has the four #428 toolchain paths.
    let info = read_build_info(project, "esp32s3");
    for key in ["nm_path", "cppfilt_path", "readelf_path", "objdump_path"] {
        let v = info
            .get(key)
            .unwrap_or_else(|| panic!("{key} present"))
            .as_str()
            .unwrap_or_else(|| panic!("{key} is a string"));
        assert!(!v.is_empty(), "{key} is non-empty");
    }
    // #428 also mirrors them into the `aliases` block.
    let aliases = info
        .get("aliases")
        .and_then(|v| v.as_object())
        .expect("aliases block present");
    for short in ["nm", "c++filt", "readelf", "objdump"] {
        assert!(aliases.contains_key(short), "aliases.{short} present");
    }

    // Step 4: bloat (no flags).
    let bloat_out = run_fbuild(&["bloat", project.to_str().unwrap()]).expect("fbuild bloat runs");
    assert!(
        bloat_out.status.success(),
        "fbuild bloat failed: stdout={}\nstderr={}",
        String::from_utf8_lossy(&bloat_out.stdout),
        String::from_utf8_lossy(&bloat_out.stderr),
    );
    let bloat_stdout = String::from_utf8_lossy(&bloat_out.stdout).into_owned();

    // Step 5: report files at the documented Phase 3 path.
    let report_dir = project
        .join(".fbuild")
        .join("build")
        .join("esp32s3")
        .join("bloat-report");
    let json_target = report_dir.join("report.json");
    let md_target = report_dir.join("report.md");
    assert!(
        json_target.is_file(),
        "expected {} to exist",
        json_target.display()
    );
    assert!(
        md_target.is_file(),
        "expected {} to exist",
        md_target.display()
    );

    // Step 6: stdout prints both absolute paths on exit (#439).
    assert!(
        bloat_stdout.contains("report.json"),
        "stdout must mention report.json"
    );
    assert!(
        bloat_stdout.contains("report.md"),
        "stdout must mention report.md"
    );

    // Step 7: content invariants from the #2773 audit. Bounds are
    // generous so FastLED master drift doesn't flake the test —
    // they pin the shape of the report, not exact bytes.
    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json_target).unwrap())
            .expect("report.json parses");
    let total_flash = report["total_flash"].as_u64().expect("total_flash u64");
    let total_ram = report["total_ram"].as_u64().expect("total_ram u64");
    let symbols = report["symbols"].as_array().expect("symbols array");
    let sections = report["sections"].as_array().expect("sections array");

    assert!(
        (300_000..500_000).contains(&total_flash),
        "expected ~388 KB flash, got {total_flash}"
    );
    assert!(
        (25_000..60_000).contains(&total_ram),
        "expected ~37 KB ram, got {total_ram}"
    );
    assert!(
        symbols.len() > 2_000,
        "expected >2000 sized symbols, got {}",
        symbols.len()
    );
    assert!(
        sections.len() > 100,
        "expected >100 per-archive sections, got {}",
        sections.len()
    );

    // Step 8: map-derived synthesis covered (#427 invariant). The
    // FastLED chipset constructor's rodata pool must be named, not
    // bucketed at object level.
    let synth_count = symbols
        .iter()
        .filter(|s| s["source"].as_str() == Some("map-derived"))
        .count();
    assert!(
        synth_count > 100,
        "expected >100 map-derived synthetic owners, got {synth_count}"
    );

    // Step 9: markdown report renders a top-flash table.
    let md = std::fs::read_to_string(&md_target).expect("read report.md");
    for needle in [
        "# Symbol analysis",
        "## Top",
        "flash symbols",
        "| Bytes |",
        "fl::",
    ] {
        assert!(
            md.contains(needle),
            "report.md missing expected fragment: {needle:?}"
        );
    }
}
