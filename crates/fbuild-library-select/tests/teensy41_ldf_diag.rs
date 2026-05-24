//! Diagnostic-only test for FastLED/fbuild#267 (Fix 1).
//!
//! Runs the LDF resolver against the real cached Teensyduino framework
//! library set plus the user's local FastLED source tree. Prints which
//! framework libraries get selected and why. Marked `#[ignore]` so it
//! only runs on explicit demand:
//!
//!     soldr cargo test -p fbuild-library-select \
//!         --test teensy41_ldf_diag -- --ignored --nocapture
//!
//! Requires:
//!   * Cached Teensyduino framework at:
//!     `~/.fbuild/prod/cache/platforms/dl-framework-arduinoteensy-*/.../1.160.0/libraries/`
//!   * Local FastLED checkout at `~/dev/fastled/`
//!
//! Skips with a printed reason when either is missing.

use std::path::{Path, PathBuf};

use fbuild_library_select::resolve_with_stats;
use fbuild_packages::library::framework_library::discover_framework_libraries;
use walkdir::WalkDir;

fn home_dir() -> Option<PathBuf> {
    std::env::var("USERPROFILE")
        .ok()
        .or_else(|| std::env::var("HOME").ok())
        .map(PathBuf::from)
}

fn find_teensy_libraries() -> Option<PathBuf> {
    let home = home_dir()?;
    let root = home
        .join(".fbuild")
        .join("prod")
        .join("cache")
        .join("platforms");
    let entries = std::fs::read_dir(&root).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("dl-framework-arduinoteensy-") {
            continue;
        }
        for hash_entry in std::fs::read_dir(entry.path()).ok()?.flatten() {
            for version_entry in std::fs::read_dir(hash_entry.path()).ok()?.flatten() {
                let libs = version_entry.path().join("libraries");
                if libs.is_dir() {
                    return Some(libs);
                }
            }
        }
    }
    None
}

fn find_fastled_src() -> Option<PathBuf> {
    let home = home_dir()?;
    let candidate = home.join("dev").join("fastled").join("src");
    if candidate.is_dir() {
        Some(candidate)
    } else {
        None
    }
}

fn collect_seeds(root: &Path) -> Vec<PathBuf> {
    let mut seeds = Vec::new();
    for entry in WalkDir::new(root).into_iter().flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let ext = entry
            .path()
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        if matches!(
            ext.as_str(),
            "c" | "cpp" | "cc" | "cxx" | "s" | "ino" | "h" | "hh" | "hpp" | "hxx"
        ) {
            seeds.push(entry.path().to_path_buf());
        }
    }
    seeds
}

#[test]
#[ignore = "needs cached Teensyduino + local FastLED; run with --ignored --nocapture"]
fn diag_teensy41_blink_what_libs_get_selected() {
    let Some(libraries_dir) = find_teensy_libraries() else {
        eprintln!("SKIP: no cached Teensyduino libraries/ dir found");
        return;
    };
    let Some(fastled_src) = find_fastled_src() else {
        eprintln!("SKIP: ~/dev/fastled/src not found");
        return;
    };

    eprintln!("libraries_dir = {}", libraries_dir.display());
    eprintln!("fastled_src   = {}", fastled_src.display());

    let libraries = discover_framework_libraries(&libraries_dir);
    eprintln!("discovered {} framework libraries", libraries.len());

    let seeds = collect_seeds(&fastled_src);
    eprintln!("collected {} FastLED seed files", seeds.len());

    let project_search_paths = vec![fastled_src.clone()];

    let (selection, stats) = resolve_with_stats(&seeds, &project_search_paths, &libraries);

    eprintln!("---");
    eprintln!(
        "RESOLVE STATS: passes={} files_read={}",
        stats.passes, stats.files_read
    );
    eprintln!(
        "REQUIRED LIBRARIES ({}):",
        selection.required_libraries.len()
    );
    for name in &selection.required_libraries {
        eprintln!("  - {name}");
    }
    eprintln!("SOURCE FILES ({}):", selection.source_files.len());
    for src in &selection.source_files {
        eprintln!("  - {}", src.display());
    }

    let ssd1351_selected = selection
        .required_libraries
        .iter()
        .any(|n| n.eq_ignore_ascii_case("ssd1351"));
    eprintln!(
        "---\nssd1351 SELECTED? {}",
        if ssd1351_selected {
            "YES (leak — bug)"
        } else {
            "no"
        }
    );

    // ---- Scenario B: project has NO local FastLED visible to the walker,
    // only an example sketch that does `#include <FastLED.h>`. This is
    // what happens if the per-example build's src_dir doesn't include the
    // repo src. Walker falls through to bundled FastLED → expect leak.
    eprintln!("---\n=== Scenario B: sketch-only project (no local FastLED in roots) ===");
    let tmp = tempfile::tempdir().expect("tempdir");
    let sketch_src = tmp.path().join("src");
    std::fs::create_dir_all(&sketch_src).unwrap();
    std::fs::write(
        sketch_src.join("Blink.ino"),
        "#include <FastLED.h>\nvoid setup(){} void loop(){}\n",
    )
    .unwrap();
    let sketch_seeds = collect_seeds(&sketch_src);
    let sketch_paths = vec![sketch_src.clone()];
    let (sketch_sel, sketch_stats) = resolve_with_stats(&sketch_seeds, &sketch_paths, &libraries);
    eprintln!(
        "Scenario B STATS: passes={} files_read={}",
        sketch_stats.passes, sketch_stats.files_read
    );
    eprintln!(
        "Scenario B SELECTED ({}):",
        sketch_sel.required_libraries.len()
    );
    for name in &sketch_sel.required_libraries {
        eprintln!("  - {name}");
    }
    let scen_b_ssd = sketch_sel
        .required_libraries
        .iter()
        .any(|n| n.eq_ignore_ascii_case("ssd1351"));
    eprintln!(
        "Scenario B ssd1351? {}",
        if scen_b_ssd { "YES (leak)" } else { "no" }
    );
}
