//! Warm-cache library-selection bench across a curated FastLED examples matrix.
//!
//! This is the AC#5 / P-01 measurement for FastLED/fbuild#205: for each
//! example sketch under `$FASTLED_DIR/examples/`, runs the resolver cold
//! (empty `FileKvStore`) and warm (cache pre-populated by the cold call) and
//! reports the timings as a Markdown table.
//!
//! ## Inputs
//!
//! - `FASTLED_DIR` (env, required) — root of a FastLED checkout. Must
//!   contain `src/` and `examples/<Name>/<Name>.ino`. No default — see
//!   `resolve_fastled_dir` for why.
//! - Curated `EXAMPLES` list below — a representative spread, not the
//!   full ~80-example tree. Each is a single-`.ino` Arduino sketch.
//!
//! ## What it measures
//!
//! For each `(example, framework_lib_set)` pair:
//!
//! - **Cold**: open a fresh `FileKvStore`, call `resolve_cached(...)`. Wall-clock
//!   includes scanner walk, LDF reconciliation, and cache write.
//! - **Warm**: call `resolve_cached(...)` again against the same `FileKvStore`.
//!   Wall-clock includes cache-key compute (sorted seed/header hashing —
//!   bounded by `cache_key` itself) and bincode decode of the cached
//!   `Selection`. Asserts `from_cache = true` so silent re-misses surface
//!   immediately.
//!
//! The framework library set is a synthetic Teensyduino-style stub built
//! via `MiniFramework`. The bench measures resolver throughput, not
//! whether the right libs are selected — that's the acceptance-test layer
//! (`tests/teensylc_acceptance.rs`).
//!
//! ## CLI
//!
//! ```text
//! bench-fastled-examples [--json <path>] [--max-warm-ms <f64>]
//! ```
//!
//! `--json <path>` writes a structured report alongside the stdout table
//! for diffing in PR comments.
//!
//! `--max-warm-ms <f64>` enforces AC#5: each example whose warm timing
//! exceeds the threshold causes the binary to exit 1 after the table is
//! printed. Wired in CI at 50 ms (`~25x` headroom over current ~1-2 ms
//! warm numbers — absorbs runner noise without false positives).
//!
//! Refs: #205 Phase 7 (AC#5), #218.

use std::path::{Path, PathBuf};
use std::time::Instant;

use fbuild_library_select::CachedSelection;
use fbuild_library_select::cache::{CacheKeyInputs, FileKvStore, resolve_cached};
use fbuild_packages::library::FrameworkLibrary;
use fbuild_packages::library::framework_library::discover_framework_libraries;
use fbuild_test_support::MiniFramework;

/// Curated subset that spans the simple/complex spectrum without dragging
/// in every one of the ~80 FastLED examples. Bench iteration time at six
/// examples is a few seconds — adding more is cheap if needed.
const EXAMPLES: &[&str] = &[
    "Blink",
    "Pacifica",
    "Animartrix",
    "Audio",
    "BlurBenchmark",
    "Chromancer",
];

/// Synthetic Teensyduino-class framework lib names. We only need names
/// here — the resolver attributes by include-dir prefix, and these libs
/// don't need to be functionally selected for the timing to be meaningful
/// (the cost is in the walker/LDF, not the lib count).
const FRAMEWORK_LIBS: &[&str] = &["SPI", "Wire", "EEPROM", "OctoWS2811", "Audio", "RadioHead"];

struct Row {
    example: String,
    cold_ms: f64,
    warm_ms: f64,
    selected: Vec<String>,
    hit: bool,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("bench-fastled-examples: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let json_out = parse_path_flag(&args, "--json");
    let max_warm_ms = parse_f64_flag(&args, "--max-warm-ms")?;

    let fastled_dir = resolve_fastled_dir()?;
    let fastled_src = fastled_dir.join("src");
    if !fastled_src.is_dir() {
        return Err(format!(
            "FastLED checkout not found: {} (FASTLED_DIR must point at a tree with src/ + examples/)",
            fastled_dir.display()
        )
        .into());
    }

    let mut mf = MiniFramework::new();
    for name in FRAMEWORK_LIBS {
        mf.add_library(name).done();
    }
    let libraries = discover_framework_libraries(&mf.libraries_dir());
    let framework_root = mf.framework_root().to_path_buf();

    println!("# bench/fastled-examples warm-cache report");
    println!();
    println!("- FastLED: `{}`", fastled_dir.display());
    println!("- Framework lib set: {} synthetic libs", libraries.len());
    match max_warm_ms {
        Some(t) => println!("- Warm threshold: {t:.2} ms"),
        None => println!("- Warm threshold: none"),
    }
    println!();
    println!("| example | cold (ms) | warm (ms) | speedup | selected | hit |");
    println!("|---|---:|---:|---:|---:|---|");

    let mut rows: Vec<Row> = Vec::new();
    for example in EXAMPLES {
        let row = measure_example(
            example,
            &fastled_dir,
            &fastled_src,
            &framework_root,
            &libraries,
        )?;
        let speedup = if row.warm_ms > 0.0 {
            row.cold_ms / row.warm_ms
        } else {
            f64::INFINITY
        };
        println!(
            "| {} | {:.2} | {:.2} | {:.1}x | {} | yes |",
            row.example,
            row.cold_ms,
            row.warm_ms,
            speedup,
            row.selected.len(),
        );
        rows.push(row);
    }

    // Collect breaches (after the table prints, so the row data is always visible).
    let breaches: Vec<(String, f64)> = match max_warm_ms {
        Some(t) => rows
            .iter()
            .filter(|r| r.warm_ms > t)
            .map(|r| (r.example.clone(), r.warm_ms))
            .collect(),
        None => Vec::new(),
    };

    if let Some(path) = json_out {
        write_json_report(&path, &fastled_dir, &rows, max_warm_ms, &breaches)?;
        println!();
        println!("JSON report written to `{}`", path.display());
    }

    if let Some(t) = max_warm_ms {
        if !breaches.is_empty() {
            let summary: Vec<String> = breaches
                .iter()
                .map(|(name, ms)| format!("{name} ({ms:.2} ms)"))
                .collect();
            return Err(format!(
                "AC#5 warm threshold breached: {} example(s) exceeded {:.2} ms: {}",
                breaches.len(),
                t,
                summary.join(", ")
            )
            .into());
        }
    }

    Ok(())
}

fn measure_example(
    name: &str,
    fastled_dir: &Path,
    fastled_src: &Path,
    framework_root: &Path,
    libraries: &[FrameworkLibrary],
) -> Result<Row, Box<dyn std::error::Error>> {
    let ino_path = fastled_dir
        .join("examples")
        .join(name)
        .join(format!("{name}.ino"));
    if !ino_path.is_file() {
        return Err(format!("missing sketch {ino_path:?}").into());
    }

    // Root scratch dirs under `~/.fbuild/{dev|prod}/tmp/fastled-examples-bench/`
    // — FastLED/fbuild#844 bridge pair 10.
    let stage = tempfile::tempdir_in(fbuild_paths::temp_subdir("fastled-examples-bench"))?;
    let stage_src = stage.path().join("src");
    std::fs::create_dir_all(&stage_src)?;
    let main_cpp = stage_src.join("main.cpp");
    std::fs::write(&main_cpp, std::fs::read(&ino_path)?)?;

    let seeds = vec![main_cpp];
    let search_paths = vec![stage_src, fastled_src.to_path_buf()];

    let kv_dir = tempfile::tempdir_in(fbuild_paths::temp_subdir("fastled-examples-bench"))?;
    let kv = FileKvStore::open(kv_dir.path().join("kv"))?;

    let inputs = CacheKeyInputs {
        toolchain_triple: "teensy-arm-none-eabi",
        framework_install_path: framework_root,
        framework_version: "bench-fastled-examples-v1",
    };

    let (cold, cold_ms) = timed(|| resolve_cached(&seeds, &search_paths, libraries, &inputs, &kv))?;
    if cold.from_cache {
        return Err("cold call unexpectedly hit the cache".into());
    }

    let (warm, warm_ms) = timed(|| resolve_cached(&seeds, &search_paths, libraries, &inputs, &kv))?;
    if !warm.from_cache {
        return Err(format!("warm call unexpectedly missed the cache for `{name}`").into());
    }

    Ok(Row {
        example: name.to_string(),
        cold_ms,
        warm_ms,
        selected: warm.selection.required_libraries.clone(),
        hit: true,
    })
}

fn timed<F, E>(f: F) -> Result<(CachedSelection, f64), E>
where
    F: FnOnce() -> Result<CachedSelection, E>,
{
    let t0 = Instant::now();
    let res = f()?;
    Ok((res, t0.elapsed().as_secs_f64() * 1000.0))
}

fn write_json_report(
    path: &Path,
    fastled_dir: &Path,
    rows: &[Row],
    max_warm_ms: Option<f64>,
    breaches: &[(String, f64)],
) -> Result<(), Box<dyn std::error::Error>> {
    let entries: Vec<_> = rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "example": r.example,
                "cold_ms": r.cold_ms,
                "warm_ms": r.warm_ms,
                "selected": r.selected,
                "hit": r.hit,
            })
        })
        .collect();
    let breach_entries: Vec<_> = breaches
        .iter()
        .map(|(name, ms)| {
            serde_json::json!({
                "example": name,
                "warm_ms": ms,
            })
        })
        .collect();
    let body = serde_json::json!({
        "fastled_dir": fastled_dir.to_string_lossy(),
        "max_warm_ms": max_warm_ms,
        "breaches": breach_entries,
        "rows": entries,
    });
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(&body)?)?;
    Ok(())
}

fn parse_path_flag(args: &[String], flag: &str) -> Option<PathBuf> {
    let prefix = format!("{flag}=");
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == flag {
            return iter.next().map(PathBuf::from);
        }
        if let Some(rest) = arg.strip_prefix(prefix.as_str()) {
            return Some(PathBuf::from(rest));
        }
    }
    None
}

fn parse_f64_flag(args: &[String], flag: &str) -> Result<Option<f64>, Box<dyn std::error::Error>> {
    let prefix = format!("{flag}=");
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        let raw = if arg == flag {
            match iter.next() {
                Some(v) => v.as_str(),
                None => return Err(format!("{flag} requires a value").into()),
            }
        } else if let Some(rest) = arg.strip_prefix(prefix.as_str()) {
            rest
        } else {
            continue;
        };
        let parsed: f64 = raw
            .parse()
            .map_err(|e| format!("{flag} expects a number, got {raw:?}: {e}"))?;
        if !parsed.is_finite() || parsed < 0.0 {
            return Err(
                format!("{flag} must be a non-negative finite number, got {parsed}").into(),
            );
        }
        return Ok(Some(parsed));
    }
    Ok(None)
}

/// Read `FASTLED_DIR` from the environment. No fallback default: the
/// value depends on the host (CI uses a workspace-relative checkout,
/// developers use whatever convention they like) and silently
/// substituting a workstation-specific path would mask configuration
/// errors and leak the previous developer's layout into reports.
fn resolve_fastled_dir() -> Result<PathBuf, Box<dyn std::error::Error>> {
    match std::env::var("FASTLED_DIR") {
        Ok(s) if !s.is_empty() => Ok(PathBuf::from(s)),
        _ => Err(
            "FASTLED_DIR is not set. Point it at a FastLED checkout root (a directory \
             containing `src/` and `examples/`)."
                .into(),
        ),
    }
}
