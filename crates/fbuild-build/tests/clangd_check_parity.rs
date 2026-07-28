//! Headless `clangd --check` acceptance harness (FastLED/fbuild#1076).
//!
//! CI cannot launch Zed (hard DX11-GPU requirement on Windows, no display on
//! Linux/macOS runners), so the IDE-integration acceptance test is proxied
//! through clangd's own headless smoke-test mode instead: generate the
//! IDE-flavored `compile_commands.json` for a representative project (same
//! machinery `fbuild clangd-config` / `fbuild ide` use — see
//! `crates/fbuild-cli/src/cli/clangd_config/mod.rs`), confirm it contains a
//! raw-`.ino` entry per the #1197 prelude design
//! (`CompileDatabase::swap_ino_entries_for_raw`,
//! `crates/fbuild-build-engine/src/compile_database/clang.rs`), then run
//! `clangd --check=<sketch.ino>` and assert no unexpected error-severity
//! diagnostics.
//!
//! This is the "parity by construction" acceptance criterion from the
//! issue's direction-update comment: the raw-`.ino` entry carries *the same
//! flags* the generated `.ino.cpp` entry would have carried (plus
//! `-x c++ -include <prelude>`), so a clean `clangd --check` on the `.ino`
//! is definitionally a clean check of what the real build compiles — a
//! literal diff against a second `clangd --check` run on the `.ino.cpp` is
//! not needed to prove parity (and isn't attempted here; see module docs
//! on `swap_ino_entries_for_raw` for why the generated entry is removed,
//! not duplicated).
//!
//! Run locally: `soldr cargo test -p fbuild-build --test clangd_check_parity -- --ignored --nocapture`
//! (see docs/DEVELOPMENT.md → "clangd --check parity harness").

use std::path::{Path, PathBuf};
use std::time::Duration;

use fbuild_build::avr::orchestrator::AvrOrchestrator;
use fbuild_build::{BuildOrchestrator, BuildParams};
use fbuild_core::BuildProfile;

/// Wall-clock cap for the whole harness (compiledb generation + clangd
/// indexing). Generous — cold AVR toolchain/core downloads can take a few
/// minutes; a warm cache (the common case once #1194-era CI has run once)
/// finishes in seconds. Mirrors the pattern in
/// `crates/fbuild-build/tests/avr_build.rs` (FastLED/fbuild#806).
const HARNESS_TIMEOUT: Duration = Duration::from_secs(600);

/// Diagnostic substrings that are known-benign and must never fail the
/// assertion even though they contain the literal text "error:". Empty
/// today — add an entry here (with a comment citing the upstream cause)
/// if a future clangd/toolchain combination produces a spurious one that
/// isn't worth chasing.
const ERROR_ALLOWLIST: &[&str] = &[];

async fn under_timeout<F: std::future::Future>(fut: F) -> F::Output {
    match tokio::time::timeout(HARNESS_TIMEOUT, fut).await {
        Ok(v) => v,
        Err(_) => panic!(
            "clangd --check parity harness exceeded {:.0}s budget — see FastLED/fbuild#1076",
            HARNESS_TIMEOUT.as_secs_f64()
        ),
    }
}

/// Absolute path to `tests/platform/uno` (the AVR representative project —
/// the lightest of the platforms fbuild ships: no ESP32/ARM toolchain
/// download, and its AVR toolchain + Arduino core are commonly already
/// warm in `~/.fbuild/*/cache/` from other AVR-tagged tests).
fn uno_project_dir() -> PathBuf {
    // Deliberately not `.canonicalize()`d: on Windows that prepends the
    // `\\?\` extended-length-path prefix, which clangd's own path parsing
    // does not expect in `--check=<file>` and would turn every diagnostic
    // location mismatch into a silent no-op match instead of a real
    // assertion. `CARGO_MANIFEST_DIR` is already absolute, so the `..`
    // components resolve fine for both filesystem calls and the path
    // strings baked into `compile_commands.json`.
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/platform/uno");
    assert!(
        dir.is_dir(),
        "tests/platform/uno must exist at {}",
        dir.display()
    );
    dir
}

/// Find `clangd` (or `clangd.exe` on Windows) on `PATH`. Returns `None`
/// rather than erroring — the caller uses this to skip cleanly, since
/// fbuild deliberately does not manage/download a clangd binary itself
/// (Zed manages its own; see FastLED/fbuild#1076 research comment §1.5 —
/// "Zed manages its own clangd binary by default, so we don't have to
/// distribute clangd"). Compare `fbuild_toolchain::toolchain::clang`,
/// which *does* manage `clang`/`clang-tidy`/`iwyu` via
/// `ClangComponent::get_binary` — clangd is intentionally not one of
/// `ClangComponentKind`'s variants.
fn find_clangd_on_path() -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    let exe_name = if cfg!(windows) {
        "clangd.exe"
    } else {
        "clangd"
    };
    std::env::split_paths(&path_var)
        .map(|dir| dir.join(exe_name))
        .find(|candidate| candidate.is_file())
}

/// Diagnostic lines that look like a clang error-severity diagnostic
/// (`<file>:<line>:<col>: error: <message>` embedded in clangd's own log
/// line), filtered against [`ERROR_ALLOWLIST`].
fn unexpected_error_diagnostics(output: &str) -> Vec<&str> {
    output
        .lines()
        .filter(|line| line.contains(": error:") || line.contains(" error:"))
        .filter(|line| !ERROR_ALLOWLIST.iter().any(|allowed| line.contains(allowed)))
        .collect()
}

/// Generate the IDE-flavored `compile_commands.json` for `tests/platform/uno`
/// via the same in-process orchestrator path `avr_build.rs` uses (no daemon,
/// no CLI subprocess — `compiledb_only: true` skips actual compilation and
/// linking, so this is the "lighter path" mentioned in FastLED/fbuild#1076:
/// generate the DB without paying for a full AVR link).
///
/// Writes only into a temp `build_dir` plus the project's own
/// `compile_commands.json` (gitignored — see `.gitignore`), matching what a
/// normal `fbuild build -t compiledb` against this project already does.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires installed toolchains and clangd on PATH (#1076 parity harness)"]
async fn clangd_check_parity_uno() {
    let project_dir = uno_project_dir();

    let clangd_path = match find_clangd_on_path() {
        Some(p) => p,
        None => {
            eprintln!(
                "SKIP: clangd not found on PATH — install it (e.g. via Zed, \
                 `winget install LLVM.LLVM`, `apt install clangd`, `brew install llvm`) \
                 to run this harness. See docs/DEVELOPMENT.md."
            );
            return;
        }
    };
    eprintln!("Using clangd: {}", clangd_path.display());

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let build_dir = tmp.path().join(".fbuild/build/uno/release");

    let params = BuildParams {
        project_dir: project_dir.clone(),
        env_name: "uno".to_string(),
        clean_all: false,
        clean_only: false,
        clean: false,
        profile: BuildProfile::Release,
        build_dir,
        verbose: true,
        jobs: None,
        generate_compiledb: true,
        compiledb_only: true,
        log_sender: None,
        symbol_analysis: false,
        symbol_analysis_path: None,
        no_timestamp: true,
        src_dir: None,
        pio_env: Default::default(),
        extra_build_flags: Vec::new(),
        watch_set_cache: None,
        bloat_analysis: false,
    };

    let orchestrator = AvrOrchestrator;
    let result = under_timeout(orchestrator.build(&params))
        .await
        .expect("compiledb-only AVR build should succeed");
    assert!(result.success, "compiledb generation should report success");

    let db_path = result
        .compile_database_path
        .expect("compiledb_only build must produce a compile_commands.json path");
    assert!(db_path.exists(), "{} must exist", db_path.display());

    let db_json = std::fs::read_to_string(&db_path).expect("read compile_commands.json");
    let entries: Vec<serde_json::Value> =
        serde_json::from_str(&db_json).expect("compile_commands.json must be valid JSON");
    assert!(!entries.is_empty(), "compile database must not be empty");

    // Locate the raw `.ino` entry (#1197's swap: the generated `<stem>.ino.cpp`
    // entry is replaced by one entry per raw `.ino` tab with
    // `-x c++ -include <prelude>`). This is the core regression check: if
    // `swap_ino_entries_for_raw` ever stops firing (e.g. `ino_preludes`
    // silently becomes empty), the harness fails here before ever touching
    // clangd — a `.ino.cpp`-only DB is the exact bug #1076 exists to fix.
    let ino_entry = entries
        .iter()
        .find(|e| {
            e["file"]
                .as_str()
                .map(|f| f.ends_with(".ino") && !f.ends_with(".ino.cpp"))
                .unwrap_or(false)
        })
        .unwrap_or_else(|| {
            panic!(
                "compile_commands.json has no raw-.ino entry (only generated .ino.cpp?) \
                 — swap_ino_entries_for_raw regression. entries: {:#?}",
                entries
            )
        });

    let sketch_path = ino_entry["file"]
        .as_str()
        .expect("entry.file must be a string");
    assert!(
        !entries
            .iter()
            .any(|e| e["file"].as_str() == Some(&format!("{sketch_path}.cpp"))),
        "generated .ino.cpp entry must have been removed by swap_ino_entries_for_raw, not duplicated"
    );

    let arguments = ino_entry["arguments"]
        .as_array()
        .expect("entry.arguments must be an array");
    let arg_strs: Vec<&str> = arguments.iter().filter_map(|a| a.as_str()).collect();
    assert!(
        arg_strs.contains(&"-include"),
        "raw-.ino entry must carry -include <prelude> per the #1197 prelude design: {:?}",
        arg_strs
    );
    let prelude_index = arg_strs
        .iter()
        .position(|a| *a == "-include")
        .expect("checked above")
        + 1;
    let prelude_path = arg_strs[prelude_index];
    assert!(
        Path::new(prelude_path).exists(),
        "prelude header {} referenced by -include must exist",
        prelude_path
    );
    eprintln!("Raw .ino entry: {sketch_path}");
    eprintln!("Prelude header: {prelude_path}");

    // Run `clangd --check=<sketch.ino>` headlessly against the DB we just
    // generated. `--compile-commands-dir` is passed explicitly rather than
    // relying on a `.clangd` file's `CompilationDatabase: .` discovery, so
    // this harness has no dependency on `fbuild clangd-config` having been
    // run first.
    let mut cmd = tokio::process::Command::new(&clangd_path);
    cmd.arg(format!("--check={sketch_path}"))
        .arg(format!("--compile-commands-dir={}", project_dir.display()))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let output = under_timeout(async {
        cmd.output()
            .await
            .expect("failed to spawn clangd — found on PATH but could not execute")
    })
    .await;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    eprintln!("--- clangd --check stdout ---\n{stdout}");
    eprintln!("--- clangd --check stderr ---\n{stderr}");
    eprintln!("--- clangd exit status: {:?} ---", output.status.code());

    // clangd --check logs diagnostics (including error-severity ones) to its
    // own log stream rather than always failing the process on findings, so
    // assert on diagnostic content rather than solely on exit code — a
    // nonzero exit is reported for visibility but is not itself the failure
    // condition (clangd can exit nonzero on internal issues unrelated to
    // sketch diagnostics, and can exit 0 while still having logged errors).
    let combined = format!("{stdout}\n{stderr}");
    let unexpected = unexpected_error_diagnostics(&combined);
    assert!(
        unexpected.is_empty(),
        "clangd --check reported unexpected error-severity diagnostics on the raw .ino \
         (the include/define/target correctness the IDE depends on is broken):\n{}",
        unexpected.join("\n")
    );
}
