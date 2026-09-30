//! TDD gates for issue #236 (parallel walker + scan memoization + tracing).
//!
//! These tests target the public API contracts that #236 introduces:
//!
//! 1. `resolve_with_stats()` exposes a `files_read` counter so we can assert
//!    that Pass 2's re-walk reuses Pass 1's scan cache instead of re-reading
//!    every file. Contract: each reachable file is read exactly once across
//!    all passes within a single `resolve()` call.
//!
//! 2. `resolve()` emits `ldf_pass` and `ldf_walk` tracing spans so per-pass
//!    timing is visible in the daemon log without external profilers.

use std::fs;
use std::path::Path;

use fbuild_library_select::{resolve, resolve_with_stats};
use fbuild_packages::library::FrameworkLibrary;
use tempfile::TempDir;
use tracing_test::traced_test;

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn make_lib(tmp: &Path, name: &str) -> FrameworkLibrary {
    let dir = tmp.join("libraries").join(name);
    let src = dir.join("src");
    fs::create_dir_all(&src).unwrap();
    FrameworkLibrary {
        name: name.to_string(),
        dir,
        include_dirs: vec![src],
        source_files: Vec::new(),
    }
}

/// Scenario that forces ≥ 2 passes of LDF reconciliation:
///   project/main.cpp → <SPI.h>                    (pass 1 selects SPI)
///   libs/SPI/src/SPI.h                            (silent — pass 1 stops)
///   libs/SPI/src/SPI.cpp → <Wire.h>               (pass 2 selects Wire)
///   libs/Wire/src/Wire.{h,cpp}                    (pass 3 converges)
///
/// Without memoization, every pass re-reads main.cpp + SPI.h. With the
/// `WalkState`-shared scan cache, each file is read exactly once.
#[test]
fn pass2_reuses_pass1_scan_results_no_re_reads() {
    let tmp = TempDir::new().unwrap();
    let project_src = tmp.path().join("project").join("src");
    let main = project_src.join("main.cpp");
    write(&main, "#include <SPI.h>\n");

    let mut spi = make_lib(tmp.path(), "SPI");
    write(&spi.include_dirs[0].join("SPI.h"), "// silent\n");
    let spi_cpp = spi.include_dirs[0].join("SPI.cpp");
    write(&spi_cpp, "#include <Wire.h>\n");
    spi.source_files.push(spi_cpp);

    let mut wire = make_lib(tmp.path(), "Wire");
    write(&wire.include_dirs[0].join("Wire.h"), "// empty\n");
    let wire_cpp = wire.include_dirs[0].join("Wire.cpp");
    write(&wire_cpp, "// empty\n");
    wire.source_files.push(wire_cpp);

    let seeds = vec![main];
    let (selection, stats) = resolve_with_stats(&seeds, &[project_src], &[spi, wire]);

    assert_eq!(
        selection.required_libraries,
        vec!["SPI".to_string(), "Wire".to_string()],
        "two-pass reconciliation must select both SPI and Wire"
    );
    assert!(
        stats.passes >= 2,
        "scenario must require at least 2 passes (got passes={})",
        stats.passes
    );

    // The contract: each reachable file is physically read exactly once
    // across all passes inside a single `resolve()` call. files_read counts
    // std::fs::read_to_string invocations; included_files contains every
    // unique reached file. With memoization they are equal.
    assert_eq!(
        stats.files_read,
        selection.included_files.len(),
        "each reachable file must be read exactly once across all passes \
         (files_read={}, included_files.len()={})",
        stats.files_read,
        selection.included_files.len()
    );
}

/// `resolve()` must wrap its walks in tracing spans so per-pass timing is
/// observable. The walker also emits its own `ldf_walk` span around each
/// BFS invocation.
#[traced_test]
#[test]
fn resolve_emits_ldf_pass_and_ldf_walk_spans() {
    let tmp = TempDir::new().unwrap();
    let project_src = tmp.path().join("project").join("src");
    write(&project_src.join("main.cpp"), "#include <SPI.h>\n");

    let mut spi = make_lib(tmp.path(), "SPI");
    write(&spi.include_dirs[0].join("SPI.h"), "");
    let spi_cpp = spi.include_dirs[0].join("SPI.cpp");
    write(&spi_cpp, "");
    spi.source_files.push(spi_cpp);

    let seeds = vec![project_src.join("main.cpp")];
    let _ = resolve(&seeds, &[project_src], &[spi]);

    assert!(
        logs_contain("ldf_pass"),
        "expected ldf_pass span/event in tracing capture",
    );
    assert!(
        logs_contain("ldf_walk"),
        "expected ldf_walk span/event in tracing capture",
    );
}
