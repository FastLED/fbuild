//! Lint test: ensure compiler backends sanitize flags for direct execution.
//!
//! This test scans compiler source files to verify that all non-Windows code
//! paths (direct exec via `run_command`) use `prepare_flags_for_exec` to strip
//! backslash-escaped quotes from `-D` define flags.
//!
//! Background: GCC receives argv elements literally when invoked via
//! `Command::new()`. Flags like `-DARDUINO_BOARD=\"ESP32_DEV\"` cause
//! "stray '\\' in program" errors because the backslash is a literal character,
//! not a shell escape. `prepare_flags_for_exec` strips `\"` → `"`.
//!
//! Run with: `uv run soldr cargo test -p fbuild-build --test flag_escaping_lint`

use std::fs;
use std::path::{Path, PathBuf};

/// Find the crate source directory.
fn crate_src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Recursively collect all `.rs` files under a directory.
fn collect_rs_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(collect_rs_files(&path));
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                files.push(path);
            }
        }
    }
    files
}

/// Check that a compiler file that has a non-Windows `run_command` path
/// also calls `prepare_flags_for_exec` in that path.
///
/// Heuristic: if a file contains both `run_command` and `cfg!(windows)` (the
/// response-file branch pattern), it MUST also contain `prepare_flags_for_exec`.
#[test]
fn compiler_backends_must_sanitize_flags_for_exec() {
    let src = crate_src_dir();
    let mut rs_files = collect_rs_files(&src);

    // Also scan fbuild-packages which has its own library compiler.
    let packages_src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("fbuild-packages")
        .join("src");
    rs_files.extend(collect_rs_files(&packages_src));

    let mut violations = Vec::new();

    for path in &rs_files {
        let content = fs::read_to_string(path).unwrap();

        // Only check compiler files that have the Windows/non-Windows branching
        // pattern (response file vs direct exec).
        let has_run_command = content.contains("run_command");
        let has_response_file =
            content.contains("write_response_file") || content.contains("@response");
        let has_cfg_windows = content.contains("cfg!(windows)");

        // Linker files use response files for link flags (not -D defines),
        // so they don't need prepare_flags_for_exec.
        let is_linker = path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().contains("linker"));

        // If this file uses response files on Windows and run_command, it's a
        // compiler backend that must sanitize flags on the non-Windows path.
        if has_run_command
            && has_response_file
            && has_cfg_windows
            && !is_linker
            && !content.contains("prepare_flags_for_exec")
        {
            let rel = path.strip_prefix(&src).unwrap_or(path);
            violations.push(format!(
                "  {} uses run_command + response file branching but does NOT call \
                 prepare_flags_for_exec",
                rel.display()
            ));
        }
    }

    assert!(
        violations.is_empty(),
        "Flag escaping lint violations found!\n\n\
         Compiler backends that branch between response files (Windows) and direct \
         exec (Linux) MUST call `prepare_flags_for_exec()` on the non-Windows path \
         to strip backslash-escaped quotes from -D define flags.\n\n\
         Violations:\n{}\n\n\
         Fix: add `crate::compiler::prepare_flags_for_exec(all_flags)` in the else \
         branch of `cfg!(windows)`.",
        violations.join("\n")
    );
}

/// Ensure that `\\\"` (triple-escaped quotes used for define values) only appears
/// in known-safe locations: board.rs (canonical define source) and compiler.rs
/// (the escaping module itself).
///
/// If a new file uses `\\\"` to construct defines, it likely needs to go through
/// the canonical escaping pipeline instead.
#[test]
fn escaped_quote_usage_is_restricted() {
    let src = crate_src_dir();
    let rs_files = collect_rs_files(&src);

    // Also check the config and packages crates for the full picture
    let config_src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("fbuild-config")
        .join("src");
    let packages_src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("fbuild-packages")
        .join("src");

    let mut all_files = rs_files;
    all_files.extend(collect_rs_files(&config_src));
    all_files.extend(collect_rs_files(&packages_src));

    // These files are allowed to contain \\\" because they are the canonical
    // sources/handlers of escaped-quote define values.
    let allowed_files: &[&str] = &[
        "board.rs",            // canonical define source with \\\"
        "compiler.rs",         // escaping module (prepare_flags_for_exec, write_response_file)
        "esp32_framework.rs",  // SDK defines parser (reads \\\" from disk)
        "orchestrator.rs",     // fallback define construction (same pattern as board.rs)
        "library_compiler.rs", // response file writer (checks for \\\" to skip double-quoting)
    ];

    let mut violations = Vec::new();

    for path in &all_files {
        let filename = path.file_name().unwrap().to_string_lossy();
        if allowed_files.contains(&filename.as_ref()) {
            continue;
        }

        let content = fs::read_to_string(path).unwrap();

        // Look for \\\" in string literals (the Rust source pattern for
        // constructing escaped-quote define values).
        // We check for the literal 4-char sequence: \  \  "  (which in Rust source
        // is written as \\\\\\\" but in the actual file content is \\\").
        //
        // In the .rs file on disk, the pattern `\\\"` appears as those 3 chars.
        // This is what format!("\\\"{}\\\"", ...) looks like in source.
        for (line_no, line) in content.lines().enumerate() {
            // Skip comments and test code
            let trimmed = line.trim();
            if trimmed.starts_with("//") || trimmed.starts_with("///") {
                continue;
            }
            // Check for the raw source pattern: backslash-backslash-backslash-quote
            // which is how \\\" is written in a Rust string literal
            if line.contains("\\\\\\\"") {
                violations.push(format!(
                    "  {}:{}: contains \\\\\\\" pattern — define escaping should use \
                     the canonical sources (board.rs, esp32_framework.rs) and flow \
                     through prepare_flags_for_exec / write_response_file",
                    filename,
                    line_no + 1
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "Escaped-quote usage found outside canonical locations!\n\n\
         The pattern \\\\\\\" (backslash-escaped quotes for -D defines) should only \
         appear in board.rs, compiler.rs, and esp32_framework.rs. Other files should \
         receive already-formed flags and pass them through the escaping pipeline.\n\n\
         Violations:\n{}\n\n\
         Fix: use the canonical define sources or pass raw values through \
         CompilerBase::build_define_flags().",
        violations.join("\n")
    );
}
