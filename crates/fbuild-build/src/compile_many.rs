//! Two-stage `compile-many` primitive (FastLED/fbuild#238).
//!
//! Compiles a list of sketches against the same board with the framework +
//! library archives built **once**, then fans out per-sketch compile + link
//! across a thread pool.
//!
//! ## Design
//!
//! The naive "loop over `fbuild build`" strategy pays the orchestrator
//! startup cost N times and serializes work that could overlap. The
//! `compile-many` primitive splits that work into two stages with
//! independent parallelism knobs:
//!
//! - **Stage 1** runs the orchestrator once for the first sketch with
//!   `--framework-jobs` workers driving intra-build parallelism (e.g.
//!   parallel compile of framework `.cpp` files). This is the
//!   memory-heavy stage; keep the knob modest on constrained runners.
//!
//! - **Stage 2** fans out the remaining sketches across `--sketch-jobs`
//!   workers. Each worker reuses the framework / library archives written
//!   to disk by stage 1 (via the orchestrator's warm-build fast path and
//!   zccache), and only pays for `sketch.cpp -> .o -> link` per sketch.
//!   Each worker calls the orchestrator with `jobs = 1` since per-sketch
//!   work is small and the outer thread pool already saturates cores.
//!
//! ## Concurrent-safety
//!
//! Stage-2 workers operate concurrently on distinct project directories.
//! Two guarantees keep that race-free:
//!
//! 1. **Per-sketch output directories are unique.** The orchestrator
//!    derives the build root from `<project_dir>/.fbuild/build/<env>/<profile>/`,
//!    and each sketch in the input list has its own `project_dir`, so
//!    no two workers can touch the same `firmware.elf`.
//!
//! 2. **The zccache compile-cache wrapper is lock-free on the hot path.**
//!    Each worker invokes `zccache wrap <gcc> ...` as a child process;
//!    upstream zccache uses SQLite WAL + content-addressed object files
//!    so concurrent reads of the same key never block, and concurrent
//!    writes of distinct keys never block. fbuild itself adds no
//!    in-process locks around zccache (see `crates/fbuild-build/src/zccache.rs`),
//!    so stage-2 contention is bounded by the zccache daemon's own
//!    concurrency model — well below the parallelism cap we set.
//!
//! ## Routing through the existing orchestrator
//!
//! Each sketch is built through `fbuild_build::get_orchestrator(platform)`,
//! so all platform-specific behavior (toolchain resolution, framework
//! installation, LDF, link flags, size reporting) lives in one place and
//! `compile-many` automatically picks up future per-platform work without
//! re-implementing it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use fbuild_core::{BuildProfile, FbuildError, Platform, Result};

use crate::{get_orchestrator, BuildParams, BuildResult};

/// Default for `--framework-jobs` when not specified: `min(cores, 2)`.
///
/// Framework compilation is memory-heavy; a 2-core `ubuntu-latest` runner
/// can OOM with more. Beefier runners benefit from manually cranking this
/// up via the CLI flag.
pub fn default_framework_jobs() -> usize {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    cores.clamp(1, 2)
}

/// Default for `--sketch-jobs` when not specified: `cores`.
///
/// Per-sketch work (sketch.cpp compile + link against pre-built archives)
/// has a tiny memory footprint per worker, so fanning out to one worker per
/// physical core is safe even on small runners.
pub fn default_sketch_jobs() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .max(1)
}

/// Request parameters for [`compile_many`].
#[derive(Debug, Clone)]
pub struct CompileManyRequest {
    /// Board id (e.g. "uno", "teensy41"). Used to pick the matching
    /// environment within each sketch's `platformio.ini` and to dispatch
    /// to the right platform orchestrator.
    pub board: String,
    /// One project directory per sketch. Each must contain a
    /// `platformio.ini` with an environment whose `board = <board>` (or
    /// an environment literally named `<board>`).
    pub sketches: Vec<PathBuf>,
    /// Parallelism for stage 1 (framework + library compile).
    /// `None` -> [`default_framework_jobs`].
    pub framework_jobs: Option<usize>,
    /// Parallelism for stage 2 (per-sketch compile + link).
    /// `None` -> [`default_sketch_jobs`].
    pub sketch_jobs: Option<usize>,
    /// Build profile (release / quick).
    pub profile: BuildProfile,
    /// Verbose compiler output.
    pub verbose: bool,
    /// `PLATFORMIO_*` env-var overlay forwarded to each per-sketch
    /// `BuildParams.pio_env`. Empty by default. Used by `fbuild ci` to
    /// surface `--lib` / `--project-conf` to the underlying orchestrator.
    pub pio_env: HashMap<String, String>,
}

/// Result for a single sketch.
#[derive(Debug, Clone)]
pub struct SketchResult {
    /// Sketch project directory (matches the request entry).
    pub sketch: PathBuf,
    /// Resolved environment name within the sketch's `platformio.ini`.
    pub env_name: String,
    /// Whether the sketch built successfully.
    pub success: bool,
    /// Path to the produced firmware (hex/bin/uf2) when successful.
    pub firmware_path: Option<PathBuf>,
    /// Path to the produced ELF when available.
    pub elf_path: Option<PathBuf>,
    /// Build wall-clock seconds.
    pub build_time_secs: f64,
    /// Path to the per-sketch log file (text dump of `BuildLog`).
    pub log_path: Option<PathBuf>,
    /// Human-readable summary message.
    pub message: String,
    /// Stage that produced this result.
    pub stage: Stage,
}

/// Which stage compiled a sketch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// First sketch: built sequentially to warm framework/library archives.
    Stage1Framework,
    /// Subsequent sketches: built concurrently against pre-built archives.
    Stage2Sketch,
}

/// Final result of a `compile-many` invocation.
#[derive(Debug, Clone)]
pub struct CompileManyResult {
    /// One entry per requested sketch, in input order.
    pub results: Vec<SketchResult>,
    /// `true` iff every sketch built successfully.
    pub all_success: bool,
    /// Number of sketches processed by stage 1.
    pub stage1_count: usize,
    /// Number of sketches processed by stage 2.
    pub stage2_count: usize,
    /// Wall-clock for stage 1.
    pub stage1_secs: f64,
    /// Wall-clock for stage 2.
    pub stage2_secs: f64,
    /// Wall-clock for the entire `compile-many` call.
    pub total_secs: f64,
}

impl CompileManyResult {
    /// Lookup result by sketch path.
    pub fn get(&self, sketch: &Path) -> Option<&SketchResult> {
        self.results.iter().find(|r| r.sketch == sketch)
    }

    /// Map of sketch path -> success/log_path, suitable for bench summaries.
    pub fn as_map(&self) -> HashMap<PathBuf, (bool, Option<PathBuf>)> {
        self.results
            .iter()
            .map(|r| (r.sketch.clone(), (r.success, r.log_path.clone())))
            .collect()
    }
}

/// Resolve the environment name inside `platformio.ini` for the given board.
///
/// Priority:
///   1. An environment literally named `<board>`.
///   2. The first environment whose `board = <board>`.
///
/// Returns `Err` when neither is found — this is a contract violation that
/// should surface immediately rather than guessing.
fn resolve_env_for_board(project_dir: &Path, board: &str) -> Result<String> {
    let ini_path = project_dir.join("platformio.ini");
    let config = fbuild_config::PlatformIOConfig::from_path(&ini_path)?;

    if config.has_environment(board) {
        return Ok(board.to_string());
    }
    for env in config.get_environments() {
        if let Ok(env_config) = config.get_env_config(env) {
            if env_config.get("board").map(|s| s.as_str()) == Some(board) {
                return Ok(env.to_string());
            }
        }
    }
    Err(FbuildError::ConfigError(format!(
        "no environment in {} matches board '{}' (looked for [env:{}] or board={})",
        ini_path.display(),
        board,
        board,
        board
    )))
}

/// Determine the platform for a board id.
fn platform_for_board(board: &str) -> Result<Platform> {
    let cfg = fbuild_config::BoardConfig::from_board_id(board, &HashMap::new())?;
    cfg.platform().ok_or_else(|| {
        FbuildError::ConfigError(format!(
            "could not determine platform for board '{}' (mcu '{}')",
            board, cfg.mcu
        ))
    })
}

/// Build a single sketch through the platform orchestrator.
///
/// `jobs` controls intra-build parallelism (passed through to the
/// orchestrator's per-build thread pool).
fn build_one_sketch(inputs: SketchBuildInputs) -> SketchResult {
    let SketchBuildInputs {
        sketch,
        env_name,
        platform,
        profile,
        jobs,
        verbose,
        stage,
        pio_env,
    } = inputs;
    let start = Instant::now();
    let build_dir = fbuild_packages::Cache::new(&sketch).get_build_dir(&env_name, profile);
    let params = BuildParams {
        project_dir: sketch.clone(),
        env_name: env_name.clone(),
        clean: false,
        profile,
        build_dir,
        verbose,
        jobs: Some(jobs),
        generate_compiledb: false,
        compiledb_only: false,
        log_sender: None,
        symbol_analysis: false,
        symbol_analysis_path: None,
        no_timestamp: true,
        src_dir: None,
        pio_env: pio_env.into_iter().collect(),
        extra_build_flags: Vec::new(),
        watch_set_cache: None,
    };

    let outcome = match get_orchestrator(platform) {
        Ok(orch) => orch.build(&params),
        Err(e) => Err(e),
    };

    match outcome {
        Ok(br) => {
            let BuildResult {
                success,
                firmware_path,
                elf_path,
                build_time_secs,
                message,
                build_log,
                ..
            } = br;
            let log_path = write_log_file(&sketch, &env_name, profile, build_log);
            SketchResult {
                sketch,
                env_name,
                success,
                firmware_path,
                elf_path,
                build_time_secs,
                log_path,
                message,
                stage,
            }
        }
        Err(e) => SketchResult {
            sketch,
            env_name,
            success: false,
            firmware_path: None,
            elf_path: None,
            build_time_secs: start.elapsed().as_secs_f64(),
            log_path: None,
            message: format!("build error: {}", e),
            stage,
        },
    }
}

/// Persist a per-sketch log file alongside the build artifacts so the
/// bench summary / caller can fish out the full output without holding it
/// in memory across N parallel workers.
fn write_log_file(
    sketch: &Path,
    env_name: &str,
    profile: BuildProfile,
    build_log: fbuild_core::BuildLog,
) -> Option<PathBuf> {
    let build_dir = fbuild_packages::Cache::new(sketch).get_build_dir(env_name, profile);
    if std::fs::create_dir_all(&build_dir).is_err() {
        return None;
    }
    let log_path = build_dir.join("compile_many.log");
    let body = build_log.into_lines().join("\n");
    match std::fs::write(&log_path, body) {
        Ok(()) => Some(log_path),
        Err(_) => None,
    }
}

/// Inputs to a single sketch build (consumed by [`SketchBuilder`]).
#[derive(Debug, Clone)]
pub struct SketchBuildInputs {
    pub sketch: PathBuf,
    pub env_name: String,
    pub platform: Platform,
    pub profile: BuildProfile,
    pub jobs: usize,
    pub verbose: bool,
    pub stage: Stage,
    /// `PLATFORMIO_*` env-var overlay forwarded to `BuildParams.pio_env`.
    pub pio_env: HashMap<String, String>,
}

/// Trait used by [`compile_many_with`] to run a single sketch. Tests
/// inject a mock implementation that records stage / concurrency / output
/// path uniqueness without dragging in a real toolchain.
pub trait SketchBuilder: Sync {
    fn build(&self, inputs: SketchBuildInputs) -> SketchResult;
}

/// Production [`SketchBuilder`] that drives the real platform
/// orchestrator. This is the only place `get_orchestrator` is touched
/// from `compile_many`, so tests can swap it out wholesale.
pub struct OrchestratorBuilder;

impl SketchBuilder for OrchestratorBuilder {
    fn build(&self, inputs: SketchBuildInputs) -> SketchResult {
        build_one_sketch(inputs)
    }
}

/// Run the two-stage `compile-many` flow.
///
/// Returns once every sketch has been attempted. Individual sketch failures
/// do not short-circuit subsequent sketches — the caller inspects
/// [`CompileManyResult::all_success`] / [`CompileManyResult::results`].
pub fn compile_many(req: CompileManyRequest) -> Result<CompileManyResult> {
    compile_many_with(req, &OrchestratorBuilder)
}

/// Like [`compile_many`] but parameterized over the [`SketchBuilder`]
/// used to actually build each sketch. Public for tests; production
/// callers should use [`compile_many`].
pub fn compile_many_with(
    req: CompileManyRequest,
    builder: &dyn SketchBuilder,
) -> Result<CompileManyResult> {
    if req.sketches.is_empty() {
        return Err(FbuildError::Other(
            "compile_many: at least one sketch is required".to_string(),
        ));
    }

    let framework_jobs = req
        .framework_jobs
        .unwrap_or_else(default_framework_jobs)
        .max(1);
    let sketch_jobs = req.sketch_jobs.unwrap_or_else(default_sketch_jobs).max(1);
    let platform = platform_for_board(&req.board)?;

    // Pre-resolve env names + assert each sketch dir exists.  Doing this
    // up front means we never half-build the batch and leave one worker
    // crashing later with a stale path.
    let mut resolved: Vec<(PathBuf, String)> = Vec::with_capacity(req.sketches.len());
    for sketch in &req.sketches {
        if !sketch.is_dir() {
            return Err(FbuildError::Other(format!(
                "sketch project_dir does not exist: {}",
                sketch.display()
            )));
        }
        let env = resolve_env_for_board(sketch, &req.board)?;
        resolved.push((sketch.clone(), env));
    }

    let total_start = Instant::now();

    // -------- Stage 1: build the first sketch sequentially. --------
    //
    // This warms the framework / library archives and the orchestrator's
    // warm-build fingerprint cache on disk. Subsequent stage-2 workers
    // hit those caches instead of re-building the framework.
    let stage1_start = Instant::now();
    let (first_sketch, first_env) = resolved[0].clone();
    let first_result = builder.build(SketchBuildInputs {
        sketch: first_sketch,
        env_name: first_env,
        platform,
        profile: req.profile,
        jobs: framework_jobs,
        verbose: req.verbose,
        stage: Stage::Stage1Framework,
        pio_env: req.pio_env.clone(),
    });
    let stage1_secs = stage1_start.elapsed().as_secs_f64();

    // If stage 1 failed there is no point fanning out — every stage-2
    // worker would re-run the framework build (which we just proved
    // broken) and report the same error. Return what we have so far.
    if !first_result.success {
        let total_secs = total_start.elapsed().as_secs_f64();
        return Ok(CompileManyResult {
            results: vec![first_result],
            all_success: false,
            stage1_count: 1,
            stage2_count: 0,
            stage1_secs,
            stage2_secs: 0.0,
            total_secs,
        });
    }

    // -------- Stage 2: fan out the remaining sketches in parallel. --------
    let stage2_start = Instant::now();
    let rest: Vec<(PathBuf, String)> = resolved[1..].to_vec();
    let stage2_results = if rest.is_empty() {
        Vec::new()
    } else {
        run_stage2(
            &rest,
            platform,
            req.profile,
            sketch_jobs,
            req.verbose,
            builder,
            &req.pio_env,
        )
    };
    let stage2_secs = stage2_start.elapsed().as_secs_f64();

    let mut results = Vec::with_capacity(req.sketches.len());
    results.push(first_result);
    results.extend(stage2_results);

    let all_success = results.iter().all(|r| r.success);
    let stage2_count = results.len().saturating_sub(1);
    let total_secs = total_start.elapsed().as_secs_f64();

    Ok(CompileManyResult {
        results,
        all_success,
        stage1_count: 1,
        stage2_count,
        stage1_secs,
        stage2_secs,
        total_secs,
    })
}

/// Run stage-2 workers across `rest` with up to `sketch_jobs` concurrent
/// threads. Preserves input order in the returned `Vec`.
fn run_stage2(
    rest: &[(PathBuf, String)],
    platform: Platform,
    profile: BuildProfile,
    sketch_jobs: usize,
    verbose: bool,
    builder: &dyn SketchBuilder,
    pio_env: &HashMap<String, String>,
) -> Vec<SketchResult> {
    let total = rest.len();
    let cap = sketch_jobs.min(total).max(1);
    tracing::info!(
        "compile-many stage 2: {} sketches across {} workers",
        total,
        cap
    );

    // Work queue: indices into `rest`. We dispatch by index so the result
    // slot lives at a stable position regardless of completion order.
    let next = AtomicUsize::new(0);
    let mut results: Vec<Option<SketchResult>> = (0..total).map(|_| None).collect();
    let results_slot: Vec<Mutex<Option<SketchResult>>> =
        results.iter_mut().map(|_| Mutex::new(None)).collect();

    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..cap)
            .map(|_| {
                let next = &next;
                let results_slot = &results_slot;
                scope.spawn(move || loop {
                    let idx = next.fetch_add(1, Ordering::Relaxed);
                    if idx >= rest.len() {
                        break;
                    }
                    let entry = &rest[idx];
                    let (sketch, env_name) = (&entry.0, &entry.1);
                    let res = builder.build(SketchBuildInputs {
                        sketch: sketch.clone(),
                        env_name: env_name.clone(),
                        platform,
                        profile,
                        // Per-sketch work is single-TU; framework archives
                        // are already pre-built, so jobs=1 keeps memory
                        // per worker minimal.
                        jobs: 1,
                        verbose,
                        stage: Stage::Stage2Sketch,
                        pio_env: pio_env.clone(),
                    });
                    *results_slot[idx].lock().unwrap() = Some(res);
                })
            })
            .collect();
        for h in handles {
            let _ = h.join();
        }
    });

    results_slot
        .into_iter()
        .map(|slot| slot.into_inner().unwrap().expect("worker filled slot"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_framework_jobs_is_at_least_one_and_at_most_two() {
        let n = default_framework_jobs();
        assert!(n >= 1, "framework jobs must be >= 1");
        assert!(
            n <= 2,
            "framework jobs default must be capped at 2 (got {n})"
        );
    }

    #[test]
    fn default_sketch_jobs_is_at_least_one() {
        assert!(default_sketch_jobs() >= 1);
    }

    #[test]
    fn resolve_env_picks_literal_env_name() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:uno]\nplatform = atmelavr\nboard = uno\nframework = arduino\n",
        )
        .unwrap();
        assert_eq!(resolve_env_for_board(tmp.path(), "uno").unwrap(), "uno");
    }

    #[test]
    fn resolve_env_falls_back_to_board_match() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:my_custom]\nplatform = atmelavr\nboard = uno\nframework = arduino\n",
        )
        .unwrap();
        assert_eq!(
            resolve_env_for_board(tmp.path(), "uno").unwrap(),
            "my_custom"
        );
    }

    #[test]
    fn resolve_env_errors_on_no_match() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:uno]\nplatform = atmelavr\nboard = uno\nframework = arduino\n",
        )
        .unwrap();
        assert!(resolve_env_for_board(tmp.path(), "teensy41").is_err());
    }

    #[test]
    fn platform_for_board_uno_is_avr() {
        let p = platform_for_board("uno").unwrap();
        assert_eq!(p, Platform::AtmelAvr);
    }

    #[test]
    fn empty_sketch_list_errors_out() {
        let req = CompileManyRequest {
            board: "uno".to_string(),
            sketches: Vec::new(),
            framework_jobs: None,
            sketch_jobs: None,
            profile: BuildProfile::Release,
            verbose: false,
            pio_env: HashMap::new(),
        };
        assert!(compile_many(req).is_err());
    }

    #[test]
    fn missing_sketch_dir_errors_out() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("nope");
        let req = CompileManyRequest {
            board: "uno".to_string(),
            sketches: vec![missing],
            framework_jobs: None,
            sketch_jobs: None,
            profile: BuildProfile::Release,
            verbose: false,
            pio_env: HashMap::new(),
        };
        assert!(compile_many(req).is_err());
    }

    #[test]
    fn sketch_without_matching_board_errors_out() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("platformio.ini"),
            "[env:uno]\nplatform = atmelavr\nboard = uno\nframework = arduino\n",
        )
        .unwrap();
        let req = CompileManyRequest {
            board: "esp32dev".to_string(),
            sketches: vec![tmp.path().to_path_buf()],
            framework_jobs: None,
            sketch_jobs: None,
            profile: BuildProfile::Release,
            verbose: false,
            pio_env: HashMap::new(),
        };
        assert!(compile_many(req).is_err());
    }
}
