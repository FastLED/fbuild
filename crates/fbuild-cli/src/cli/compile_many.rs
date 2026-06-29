//! `fbuild compile-many` (FastLED/fbuild#238) and the thin `fbuild ci`
//! adapter that maps `pio ci` flags onto it.

use crate::output;

/// Separator used to join `PLATFORMIO_LIB_EXTRA_DIRS` entries.
///
/// PlatformIO follows `PATH`-style conventions: ';' on Windows, ':' elsewhere.
/// Centralized here so the CLI handler and the unit tests agree.
pub fn ci_lib_extra_dirs_sep() -> &'static str {
    if cfg!(windows) {
        ";"
    } else {
        ":"
    }
}

/// Map a single `pio ci` positional argument to a project directory.
///
/// `pio ci` lets callers point at either a sketch dir or directly at the
/// `.ino` file. fbuild's `compile-many` only takes project dirs, so a `.ino`
/// path is rewritten to its parent. Case-insensitive on the extension so
/// `Blink.INO` works on Windows where casing isn't preserved.
pub fn normalize_ci_sketch_entry(entry: &str) -> String {
    let path = std::path::Path::new(entry);
    let is_ino = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("ino"))
        .unwrap_or(false);
    if is_ino {
        if let Some(parent) = path.parent() {
            let p = parent.to_string_lossy().to_string();
            if p.is_empty() {
                return ".".to_string();
            }
            return p;
        }
    }
    entry.to_string()
}

/// Normalize every `pio ci` positional argument.
pub fn normalize_ci_sketches(entries: &[String]) -> Vec<String> {
    entries
        .iter()
        .map(|e| normalize_ci_sketch_entry(e))
        .collect()
}

/// Build the `PLATFORMIO_*` env overlay for `fbuild ci` from `--lib` and
/// `--project-conf`. Returns an empty map when neither flag was set.
pub fn build_ci_pio_env(
    libs: &[String],
    project_conf: Option<&str>,
) -> std::collections::HashMap<String, String> {
    let mut env = std::collections::HashMap::new();
    if !libs.is_empty() {
        let libs: Vec<String> = libs
            .iter()
            .map(|lib| {
                std::fs::canonicalize(lib)
                    .ok()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|| lib.clone())
            })
            .collect();
        env.insert(
            "PLATFORMIO_LIB_EXTRA_DIRS".to_string(),
            libs.join(ci_lib_extra_dirs_sep()),
        );
    }
    if let Some(conf) = project_conf {
        let canonical = std::fs::canonicalize(conf)
            .ok()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| conf.to_string());
        env.insert("PLATFORMIO_PROJECT_CONFIG".to_string(), canonical);
    }
    env
}

/// Handler for `fbuild compile-many` (FastLED/fbuild#238).
///
/// Runs the two-stage compile-many primitive in-process via the existing
/// `fbuild-build` orchestrator. We bypass the daemon here on purpose: the
/// design goal of #238 is "one process, one toolchain load, one LDF run,
/// one framework build" — so all stage-1 + stage-2 work happens in this
/// process, parallelism is driven by the `compile_many` thread pool, and
/// no per-sketch daemon round-trip is incurred.
pub struct CompileManyArgs {
    pub board: String,
    pub framework_jobs: Option<usize>,
    pub sketch_jobs: Option<usize>,
    pub quick: bool,
    pub release: bool,
    pub verbose: bool,
    pub diag_stage2: bool,
    pub sketches: Vec<String>,
    pub pio_env: std::collections::HashMap<String, String>,
}

pub async fn run_compile_many(args: CompileManyArgs) -> fbuild_core::Result<()> {
    use fbuild_build::compile_many::{compile_many, CompileManyRequest, Stage};

    let CompileManyArgs {
        board,
        framework_jobs,
        sketch_jobs,
        quick,
        release,
        verbose,
        diag_stage2,
        sketches,
        pio_env,
    } = args;

    let profile = if release {
        fbuild_core::BuildProfile::Release
    } else if quick {
        fbuild_core::BuildProfile::Quick
    } else {
        // Default to release: matches `fbuild build`'s default profile so
        // CI builds aren't silently dropped into quick mode.
        fbuild_core::BuildProfile::Release
    };

    let sketches: Vec<std::path::PathBuf> =
        sketches.into_iter().map(std::path::PathBuf::from).collect();

    let req = CompileManyRequest {
        board: board.clone(),
        sketches: sketches.clone(),
        framework_jobs,
        sketch_jobs,
        profile,
        verbose,
        pio_env,
        diag_stage2,
    };

    let effective_framework = req
        .framework_jobs
        .unwrap_or_else(fbuild_build::compile_many::default_framework_jobs);
    let effective_sketch = req
        .sketch_jobs
        .unwrap_or_else(fbuild_build::compile_many::default_sketch_jobs);
    output::progress(format!(
        "compile-many: board={} sketches={} framework_jobs={} sketch_jobs={}",
        board,
        sketches.len(),
        effective_framework,
        effective_sketch,
    ));

    // `compile_many` is async (driving per-stage tokio fanout). Await it
    // directly — the runtime is already multi-threaded.
    let result = compile_many(req).await?;

    // Per-sketch result map suitable for the bench summary.
    output::result("");
    output::result("compile-many results:");
    for r in &result.results {
        let stage_label = match r.stage {
            Stage::Stage1Framework => "stage1",
            Stage::Stage2Sketch => "stage2",
        };
        let status = if r.success { "OK" } else { "FAIL" };
        let log_str = r
            .log_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "-".to_string());
        output::result(format!(
            "  [{}] {} ({:.2}s) {}  log={}  {}",
            stage_label,
            status,
            r.build_time_secs,
            r.sketch.display(),
            log_str,
            r.message,
        ));
    }
    output::result("");
    output::result(format!(
        "compile-many summary: stage1={}/{:.2}s stage2={}/{:.2}s total={:.2}s",
        result.stage1_count,
        result.stage1_secs,
        result.stage2_count,
        result.stage2_secs,
        result.total_secs,
    ));
    if diag_stage2 {
        for r in result
            .results
            .iter()
            .filter(|r| r.stage == Stage::Stage2Sketch)
        {
            output::result(
                serde_json::json!({
                    "type": "stage2",
                    "worker": r.worker_index,
                    "sketch": r.sketch.display().to_string(),
                    "env": &r.env_name,
                    "success": r.success,
                    "seed_applied": r.seed_applied,
                    "seed_secs": r.seed_time_secs,
                    "build_secs": r.build_time_secs,
                    "log": r.log_path.as_ref().map(|p| p.display().to_string()),
                })
                .to_string(),
            );
        }
    }

    if !result.all_success {
        return Err(fbuild_core::FbuildError::BuildFailed(format!(
            "compile-many: {}/{} sketches failed",
            result.results.iter().filter(|r| !r.success).count(),
            result.results.len(),
        )));
    }
    Ok(())
}
