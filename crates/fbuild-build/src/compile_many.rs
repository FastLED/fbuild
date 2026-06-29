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
//!   Stage 2 splits the host's compile budget across the worker pool so
//!   a cold per-sketch framework fallback can still make progress without
//!   oversubscribing the machine.
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
//!    concurrency model â€” well below the parallelism cap we set.
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

/// Resolve the on-disk build root the orchestrator uses for a given sketch.
///
/// Routes through [`fbuild_paths::BuildLayout`] so layout decisions
/// (env-segment auto-collapse, `FBUILD_BUILD_DIR` override) match
/// what the daemon and the per-platform orchestrators use. Centralized
/// here so the stage-1â†’stage-2 cache-seeding code can address that path
/// without re-deriving the same string in three places. See
/// FastLED/fbuild#335.
pub fn project_build_dir(sketch: &Path, env: &str, profile: BuildProfile) -> PathBuf {
    fbuild_paths::BuildLayout::new(sketch.to_path_buf(), env.to_string(), profile).resolve()
}

/// Seed a stage-2 project's framework `core/` from stage 1's, so the
/// orchestrator's per-file `needs_rebuild` check (`.cmdhash` match +
/// depfile-newer-than-object) reports "already built" for every framework
/// translation unit and the worker only compiles the sketch + links.
///
/// Without this, every stage-2 project dir gets its own empty
/// `.fbuild/build/<env>/<profile>/core/`, so the framework is rebuilt
/// from scratch in every worker â€” the 25s-per-sketch path FastLED hit on
/// its teensy41 / esp32s3 runs (FastLED/fbuild#335).
///
/// Uses hardlinks to keep the seed near-free; falls back to a byte copy
/// if hardlinking fails (cross-filesystem, target FS lacks hardlink
/// support, etc.). Both modes preserve the .o mtime â€” important because
/// `needs_rebuild` consults dep-file mtimes against the .o mtime.
///
/// Idempotent: skips files that already exist at the target. A pre-
/// existing stage-2 partial build won't get clobbered.
///
/// Errors are non-fatal and tracked by the caller â€” on any failure the
/// orchestrator simply falls back to its full framework-recompile path
/// (no worse than the pre-#335 behavior).
fn seed_stage2_core_from_stage1(stage1_core: &Path, stage2_core: &Path) -> std::io::Result<()> {
    if !stage1_core.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(stage2_core)?;
    let mut n_linked = 0usize;
    let mut n_copied = 0usize;
    for entry in std::fs::read_dir(stage1_core)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if !ty.is_file() {
            // We only seed individual artifact files (.o/.d/.cmdhash). Any
            // subdir is unexpected for the framework `core/` layout, but
            // skip it defensively rather than fail the whole seed.
            continue;
        }
        let src = entry.path();
        let dst = stage2_core.join(entry.file_name());
        if dst.exists() {
            continue;
        }
        if std::fs::hard_link(&src, &dst).is_ok() {
            n_linked += 1;
        } else {
            // Hardlink failed (cross-fs, NTFS junction, etc.) â€” fall
            // back to a byte copy. `copy` preserves nothing about the
            // source mtime by default; we explicitly set it from the
            // stage-1 metadata so depfile freshness comparison still
            // returns "object newer than deps" inside the orchestrator.
            std::fs::copy(&src, &dst)?;
            if let (Ok(meta), Ok(file)) = (
                src.metadata(),
                std::fs::File::options().write(true).open(&dst),
            ) {
                if let Ok(mtime) = meta.modified() {
                    let _ = file.set_modified(mtime);
                }
            }
            n_copied += 1;
        }
    }
    tracing::info!(
        "compile-many stage 2 seed: linked {} + copied {} framework artifacts \
         from {} to {}",
        n_linked,
        n_copied,
        stage1_core.display(),
        stage2_core.display()
    );
    Ok(())
}

/// Compile parallelism to give each stage-2 worker, splitting the host's
/// available cores across `sketch_jobs` workers.
///
/// The original `jobs=1` hardcoding assumed stage-2 workers compile a
/// single TU (sketch.cpp) against a pre-built framework archive. In
/// practice consumers stage each sketch in its own project dir (so two
/// sketches with different `.ino` content can build in parallel), and
/// each project dir has its own `.fbuild/build/<env>/<profile>/` â€” which
/// means stage 2 rebuilds the framework from scratch per sketch. With
/// `jobs=1` that framework rebuild is serial inside each worker, and the
/// per-sketch wall time becomes "sum of framework TU times" instead of
/// "max of framework TU times". See FastLED/fbuild#335.
///
/// Splitting cores across workers keeps total in-flight compile slots
/// at roughly `cores` so we don't oversubscribe small runners â€” each
/// worker gets `max(1, cores / sketch_jobs)`.
pub fn stage2_jobs_per_worker(sketch_jobs: usize) -> usize {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .max(1);
    (cores / sketch_jobs.max(1)).max(1)
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
    /// Emit per-stage-2 worker diagnostics in the final result.
    pub diag_stage2: bool,
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
    /// Stage-2 worker index, when this sketch was built by stage 2.
    pub worker_index: Option<usize>,
    /// Stage-2 framework seed wall-clock seconds. Zero for stage 1 and
    /// stage-2 runs where seeding was unnecessary.
    pub seed_time_secs: f64,
    /// Whether the stage-2 core seed source existed for this sketch.
    pub seed_applied: bool,
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
/// Returns `Err` when neither is found â€” this is a contract violation that
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
///
/// `project_dir`, when provided, also allows `<project_dir>/boards/<board>.json`
/// to satisfy the lookup. This matches PlatformIO's auto-discovery of
/// project-local board manifests and is how compile_many ingests boards
/// shipped alongside a `platformio.ini` (FastLED/fbuild#515).
fn platform_for_board(board: &str, project_dir: Option<&std::path::Path>) -> Result<Platform> {
    crate::resolution::platform_for_board(board, project_dir)
}

/// Build a single sketch through the platform orchestrator.
///
/// `jobs` controls intra-build parallelism (passed through to the
/// orchestrator's per-build thread pool).
async fn build_one_sketch(inputs: SketchBuildInputs) -> SketchResult {
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
        bloat_analysis: false,
    };

    let outcome = match get_orchestrator(platform) {
        Ok(orch) => orch.build(&params).await,
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
                worker_index: None,
                seed_time_secs: 0.0,
                seed_applied: false,
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
            worker_index: None,
            seed_time_secs: 0.0,
            seed_applied: false,
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
///
/// FastLED/fbuild#820 (Phase B of #813): `build` is `async` so per-sketch
/// dispatch can `.await` the platform orchestrator's async build trait.
#[async_trait::async_trait]
pub trait SketchBuilder: Sync + Send {
    async fn build(&self, inputs: SketchBuildInputs) -> SketchResult;
}

/// Production [`SketchBuilder`] that drives the real platform
/// orchestrator. This is the only place `get_orchestrator` is touched
/// from `compile_many`, so tests can swap it out wholesale.
pub struct OrchestratorBuilder;

#[async_trait::async_trait]
impl SketchBuilder for OrchestratorBuilder {
    async fn build(&self, inputs: SketchBuildInputs) -> SketchResult {
        build_one_sketch(inputs).await
    }
}

/// Run the two-stage `compile-many` flow.
///
/// Returns once every sketch has been attempted. Individual sketch failures
/// do not short-circuit subsequent sketches â€” the caller inspects
/// [`CompileManyResult::all_success`] / [`CompileManyResult::results`].
pub async fn compile_many(req: CompileManyRequest) -> Result<CompileManyResult> {
    compile_many_with(req, &OrchestratorBuilder).await
}

/// Like [`compile_many`] but parameterized over the [`SketchBuilder`]
/// used to actually build each sketch. Public for tests; production
/// callers should use [`compile_many`].
pub async fn compile_many_with(
    req: CompileManyRequest,
    builder: &(dyn SketchBuilder + Send + Sync),
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
    // Use the first sketch's project_dir as the project-local boards/
    // search root. The convention is that `fbuild build <dir> -e <env>`
    // uses <dir> as the project_dir (it holds platformio.ini), and any
    // boards/*.json next to it should resolve. With multiple sketches in
    // one call, they typically share a parent project; the first one is
    // a good-enough default.
    let project_dir_for_boards = req.sketches.first().map(|p| p.as_path());
    let platform = platform_for_board(&req.board, project_dir_for_boards)?;

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
    // Remember stage 1's resolved (sketch_dir, env) so stage 2 can find the
    // pre-built framework artifacts and seed each worker's `core/` from
    // them â€” see `seed_stage2_core_from_stage1` and FastLED/fbuild#335.
    let stage1_sketch = first_sketch.clone();
    let stage1_env = first_env.clone();
    let first_result = builder
        .build(SketchBuildInputs {
            sketch: first_sketch,
            env_name: first_env,
            platform,
            profile: req.profile,
            jobs: framework_jobs,
            verbose: req.verbose,
            stage: Stage::Stage1Framework,
            pio_env: req.pio_env.clone(),
        })
        .await;
    let stage1_secs = stage1_start.elapsed().as_secs_f64();

    // If stage 1 failed there is no point fanning out â€” every stage-2
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
    // Stage-1 has finished and (on success) written its framework `core/`
    // artifacts to disk. Compute that path once so every stage-2 worker
    // can hardlink them into its own per-sketch `core/` and skip the
    // framework recompile entirely â€” FastLED/fbuild#335.
    let stage1_core_seed: Option<PathBuf> = if first_result.success {
        Some(project_build_dir(&stage1_sketch, &stage1_env, req.profile).join("core"))
    } else {
        None
    };
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
            stage1_core_seed.as_deref(),
            &stage1_env,
            req.diag_stage2,
        )
        .await
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
///
/// `stage1_core_seed` is the path to stage 1's framework `core/` dir; when
/// `Some` and the worker's resolved env matches `stage1_env`, the worker
/// hardlinks every framework artifact into its own per-sketch `core/`
/// before calling the orchestrator, so the framework recompile is skipped
/// (FastLED/fbuild#335). Pass `None` to disable seeding (e.g. stage 1
/// failed).
#[allow(clippy::too_many_arguments)]
async fn run_stage2(
    rest: &[(PathBuf, String)],
    platform: Platform,
    profile: BuildProfile,
    sketch_jobs: usize,
    verbose: bool,
    builder: &(dyn SketchBuilder + Send + Sync),
    pio_env: &HashMap<String, String>,
    stage1_core_seed: Option<&Path>,
    stage1_env: &str,
    diag_stage2: bool,
) -> Vec<SketchResult> {
    let total = rest.len();
    let cap = sketch_jobs.min(total).max(1);
    tracing::info!(
        "compile-many stage 2: {} sketches across {} workers",
        total,
        cap
    );

    // FastLED/fbuild#820 (Phase B of #813): replaces the old
    // `std::thread::scope` worker-pool with a `tokio::task::JoinSet`
    // gated by a semaphore. Each per-sketch task `.await`s the async
    // `SketchBuilder::build`, so the orchestrator's per-sketch
    // compile / link / size pipeline runs cooperatively on the
    // daemon's tokio runtime instead of stealing OS threads.
    let jobs_per_worker = stage2_jobs_per_worker(cap);
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(cap));
    let mut joinset: tokio::task::JoinSet<(usize, SketchResult)> = tokio::task::JoinSet::new();

    // SAFETY: the JoinSet is drained before this function returns, so the
    // borrows below stay alive for the duration of every spawned task.
    let builder_ptr: &'static (dyn SketchBuilder + Send + Sync) =
        unsafe { std::mem::transmute(builder) };
    let stage1_env_owned = stage1_env.to_string();
    let stage1_core_seed_owned: Option<PathBuf> = stage1_core_seed.map(|p| p.to_path_buf());

    for (idx, entry) in rest.iter().enumerate() {
        let sketch = entry.0.clone();
        let env_name = entry.1.clone();
        let pio_env = pio_env.clone();
        let sem = semaphore.clone();
        let seed = stage1_core_seed_owned.clone();
        let stage1_env_cloned = stage1_env_owned.clone();
        let worker_index = idx % cap;
        joinset.spawn(async move {
            let _permit = sem.acquire().await.expect("compile-many semaphore closed");
            let seed_started = Instant::now();
            let mut seed_applied = false;
            if let Some(seed_path) = seed.as_deref() {
                if env_name == stage1_env_cloned {
                    let target_core =
                        project_build_dir(&sketch, &env_name, profile).join("core");
                    seed_applied = seed_path.is_dir();
                    if let Err(e) = seed_stage2_core_from_stage1(seed_path, &target_core) {
                        tracing::warn!(
                            "compile-many stage 2: failed to seed core/ \
                             for {}: {} â€” falling back to full framework \
                             recompile",
                            sketch.display(),
                            e
                        );
                    }
                }
            }
            let seed_time_secs = seed_started.elapsed().as_secs_f64();
            let mut res = builder_ptr
                .build(SketchBuildInputs {
                    sketch: sketch.clone(),
                    env_name: env_name.clone(),
                    platform,
                    profile,
                    jobs: jobs_per_worker,
                    verbose,
                    stage: Stage::Stage2Sketch,
                    pio_env,
                })
                .await;
            res.worker_index = Some(worker_index);
            res.seed_time_secs = seed_time_secs;
            res.seed_applied = seed_applied;
            if diag_stage2 {
                tracing::info!(
                    "compile-many stage2 diag worker={} index={} sketch={} env={} seed_applied={} seed_secs={:.6} build_secs={:.6} success={}",
                    worker_index,
                    idx,
                    sketch.display(),
                    env_name,
                    seed_applied,
                    seed_time_secs,
                    res.build_time_secs,
                    res.success
                );
            }
            (idx, res)
        });
    }

    let mut results: Vec<Option<SketchResult>> = (0..total).map(|_| None).collect();
    while let Some(joined) = joinset.join_next().await {
        match joined {
            Ok((idx, res)) => results[idx] = Some(res),
            Err(e) => {
                tracing::error!("compile-many stage 2 worker join error: {e}");
            }
        }
    }
    results
        .into_iter()
        .map(|slot| slot.expect("stage-2 worker filled every slot"))
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

    /// `seed_stage2_core_from_stage1` is the foundation of the
    /// framework-archive-sharing fix for FastLED/fbuild#335. It must
    /// (a) be a no-op when the source dir is missing, (b) populate the
    /// target dir from the source, and (c) skip files that already
    /// exist at the target so a pre-existing partial stage-2 build
    /// isn't clobbered.
    #[test]
    fn seed_stage2_core_no_op_when_source_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let absent_source = tmp.path().join("missing");
        let target = tmp.path().join("target");
        assert!(seed_stage2_core_from_stage1(&absent_source, &target).is_ok());
        // Must not create the target dir for an absent source â€” otherwise
        // we'd leave litter on disk for envs that don't match.
        assert!(!target.exists());
    }

    #[test]
    fn seed_stage2_core_copies_or_links_each_file() {
        let tmp = tempfile::tempdir().unwrap();
        let stage1 = tmp.path().join("stage1");
        let stage2 = tmp.path().join("stage2");
        std::fs::create_dir_all(&stage1).unwrap();
        for (name, body) in [
            ("CDC.cpp.o", b"OBJ"),
            ("CDC.cpp.d", b"DEP"),
            ("CDC.cpp.cmdhash", b"HSH"),
        ] {
            std::fs::write(stage1.join(name), body).unwrap();
        }
        seed_stage2_core_from_stage1(&stage1, &stage2).unwrap();
        for name in ["CDC.cpp.o", "CDC.cpp.d", "CDC.cpp.cmdhash"] {
            let dst = stage2.join(name);
            assert!(dst.exists(), "expected seeded {name} at {}", dst.display());
            // Same content, regardless of whether we hardlinked or copied.
            assert_eq!(
                std::fs::read(stage1.join(name)).unwrap(),
                std::fs::read(&dst).unwrap(),
            );
        }
    }

    #[test]
    fn seed_stage2_core_skips_files_already_present_at_target() {
        // Mirrors the worker-restart case: a previous stage-2 attempt
        // partially populated the target before crashing. The seed must
        // not overwrite, because the existing file may already encode
        // local progress (e.g. the orchestrator wrote a fresher
        // `.cmdhash` while computing the same artifact).
        let tmp = tempfile::tempdir().unwrap();
        let stage1 = tmp.path().join("stage1");
        let stage2 = tmp.path().join("stage2");
        std::fs::create_dir_all(&stage1).unwrap();
        std::fs::create_dir_all(&stage2).unwrap();
        std::fs::write(stage1.join("CDC.cpp.o"), b"FROM_STAGE_1").unwrap();
        std::fs::write(stage2.join("CDC.cpp.o"), b"FROM_STAGE_2_PARTIAL").unwrap();
        seed_stage2_core_from_stage1(&stage1, &stage2).unwrap();
        assert_eq!(
            std::fs::read(stage2.join("CDC.cpp.o")).unwrap(),
            b"FROM_STAGE_2_PARTIAL"
        );
    }

    #[test]
    fn project_build_dir_matches_orchestrator_convention() {
        // Locks the on-disk convention the AVR/ESP32/etc. orchestrators
        // all derive their per-(env,profile) build root from. Any change
        // here that doesn't also update the orchestrators will silently
        // break the stage-1â†’stage-2 core/ handoff in FastLED/fbuild#335.
        let p = project_build_dir(Path::new("/tmp/sketch"), "uno", BuildProfile::Release);
        assert!(p.ends_with("sketch/.fbuild/build/uno/release"));
        let q = project_build_dir(Path::new("/tmp/sketch"), "esp32s3", BuildProfile::Quick);
        assert!(q.ends_with("sketch/.fbuild/build/esp32s3/quick"));
    }

    /// FastLED stages each board's project at
    /// `<repo>/.build/pio/<board>/` and asks fbuild to build with
    /// `env == board`. `project_build_dir` must collapse the duplicate
    /// `<board>` segment (via `BuildLayout`'s auto-detect rule) so the
    /// resulting tree fits under Windows' 260-char `MAX_PATH` limit and
    /// matches what `find_firmware` looks for. See FastLED/fbuild#432.
    #[test]
    fn project_build_dir_collapses_when_sketch_basename_matches_env() {
        let sketch = Path::new("/repo/.build/pio/teensy40");
        let p = project_build_dir(sketch, "teensy40", BuildProfile::Release);
        let s = p.to_string_lossy().to_string();
        assert!(
            !s.contains("build/teensy40/release") && !s.contains("build\\teensy40\\release"),
            "stage-2 build dir kept duplicated env segment: {s}"
        );
        assert!(p.ends_with("release"));
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
        let p = platform_for_board("uno", None).unwrap();
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
            diag_stage2: false,
        };
        assert!(compile_many(req).await.is_err());
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
            diag_stage2: false,
        };
        assert!(compile_many(req).await.is_err());
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
            diag_stage2: false,
        };
        assert!(compile_many(req).await.is_err());
    }
}
