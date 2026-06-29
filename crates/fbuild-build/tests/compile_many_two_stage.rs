//! Integration tests for the two-stage `compile-many` flow
//! (FastLED/fbuild#238).
//!
//! These tests inject a mock [`SketchBuilder`] so we exercise the
//! orchestration layer (stage counts, parallelism, output-path
//! uniqueness, input ordering) without dragging in a real toolchain.
//! Real-toolchain coverage lives in `avr_build.rs` etc. and is
//! `#[ignore]`-gated to keep `uv run test` fast.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use fbuild_build::compile_many::{
    compile_many_with, stage2_jobs_per_worker, CompileManyRequest, SketchBuildInputs,
    SketchBuilder, SketchResult, Stage,
};
use fbuild_core::BuildProfile;
use tokio::sync::Barrier;

/// Test sketch root with a minimal `platformio.ini`. Used as a
/// `project_dir` parameter — the mock builder does not read it, but
/// `compile_many` validates the file's `[env:...]` matches the requested
/// board.
fn make_sketch(parent: &Path, name: &str, board: &str) -> PathBuf {
    let dir = parent.join(name);
    std::fs::create_dir_all(&dir).expect("mkdir sketch");
    let platform = match board {
        "uno" => "atmelavr",
        "teensy41" => "teensy",
        _ => "atmelavr",
    };
    std::fs::write(
        dir.join("platformio.ini"),
        format!("[env:{board}]\nplatform = {platform}\nboard = {board}\nframework = arduino\n"),
    )
    .expect("write platformio.ini");
    dir
}

/// Mock builder that:
/// - Records every (sketch, stage, jobs) tuple it is asked to build.
/// - Writes a unique sentinel file under the canonical per-sketch
///   build_dir so we can assert no two workers race on `firmware.elf`.
/// - Optionally synchronizes stage-2 workers on a `Barrier` to prove
///   real concurrency (not just queued serial execution).
struct MockBuilder {
    /// (sketch_path, env_name, stage, jobs).
    calls: Mutex<Vec<(PathBuf, String, Stage, usize)>>,
    /// Sentinel firmware paths created during the test, used to assert
    /// per-sketch output paths are unique.
    firmware_paths: Mutex<HashSet<PathBuf>>,
    /// Optional barrier; if set, every stage-2 invocation waits on it
    /// before "completing". A successful `wait()` proves N workers
    /// were running concurrently.
    stage2_barrier: Option<Arc<Barrier>>,
    /// Bumped on every stage-2 wait completion; lets the test verify the
    /// barrier did, in fact, fire.
    stage2_wait_count: AtomicUsize,
}

impl MockBuilder {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            firmware_paths: Mutex::new(HashSet::new()),
            stage2_barrier: None,
            stage2_wait_count: AtomicUsize::new(0),
        }
    }

    fn with_barrier(barrier: Arc<Barrier>) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            firmware_paths: Mutex::new(HashSet::new()),
            stage2_barrier: Some(barrier),
            stage2_wait_count: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> Vec<(PathBuf, String, Stage, usize)> {
        self.calls.lock().unwrap().clone()
    }

    fn firmware_paths(&self) -> HashSet<PathBuf> {
        self.firmware_paths.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl SketchBuilder for MockBuilder {
    async fn build(&self, inputs: SketchBuildInputs) -> SketchResult {
        // Synthesize the canonical per-sketch firmware path the same
        // way the real orchestrator does. We deliberately use the
        // same naming convention so the uniqueness assertion is
        // meaningful — two workers racing on identical paths is the
        // bug we are trying to rule out.
        let build_dir = inputs
            .sketch
            .join(".fbuild")
            .join("build")
            .join(&inputs.env_name)
            .join(match inputs.profile {
                BuildProfile::Release => "release",
                BuildProfile::Quick => "quick",
            });
        std::fs::create_dir_all(&build_dir).expect("mkdir build_dir");
        let firmware = build_dir.join("firmware.elf");
        // Write a tiny sentinel and assert no collision happened.
        std::fs::write(&firmware, inputs.sketch.to_string_lossy().as_bytes())
            .expect("write firmware sentinel");
        {
            let mut paths = self.firmware_paths.lock().unwrap();
            assert!(
                paths.insert(firmware.clone()),
                "duplicate firmware path observed (race on {})",
                firmware.display()
            );
        }

        {
            let mut calls = self.calls.lock().unwrap();
            calls.push((
                inputs.sketch.clone(),
                inputs.env_name.clone(),
                inputs.stage,
                inputs.jobs,
            ));
        }

        // Force real overlap among stage-2 workers when a barrier is
        // wired up: every worker blocks here until N peers arrive.
        if inputs.stage == Stage::Stage2Sketch {
            if let Some(ref b) = self.stage2_barrier {
                b.wait().await;
                self.stage2_wait_count.fetch_add(1, Ordering::Relaxed);
            }
        }

        SketchResult {
            sketch: inputs.sketch.clone(),
            env_name: inputs.env_name.clone(),
            success: true,
            firmware_path: Some(firmware),
            elf_path: None,
            build_time_secs: 0.0,
            log_path: None,
            message: "mock build ok".to_string(),
            stage: inputs.stage,
            worker_index: None,
            seed_time_secs: 0.0,
            seed_applied: false,
        }
    }
}

fn make_request(
    sketches: Vec<PathBuf>,
    framework_jobs: usize,
    sketch_jobs: usize,
) -> CompileManyRequest {
    CompileManyRequest {
        board: "uno".to_string(),
        sketches,
        framework_jobs: Some(framework_jobs),
        sketch_jobs: Some(sketch_jobs),
        profile: BuildProfile::Release,
        verbose: false,
        pio_env: std::collections::HashMap::new(),
        diag_stage2: false,
    }
}

/// AC: stage 1 runs exactly once across N sketches, stage 2 produces
/// N-1 firmware artifacts, and per-sketch output paths are unique.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stage1_runs_exactly_once_and_stage2_handles_the_rest() {
    let tmp = tempfile::tempdir().unwrap();
    let sketches: Vec<PathBuf> = (0..5)
        .map(|i| make_sketch(tmp.path(), &format!("sketch{i}"), "uno"))
        .collect();

    let mock = MockBuilder::new();
    let result = compile_many_with(make_request(sketches.clone(), 1, 4), &mock)
        .await
        .expect("compile_many");

    assert!(result.all_success, "all mock builds should succeed");
    assert_eq!(result.results.len(), 5);
    assert_eq!(result.stage1_count, 1, "exactly one stage-1 invocation");
    assert_eq!(result.stage2_count, 4, "four stage-2 invocations");

    let calls = mock.calls();
    let stage1: Vec<_> = calls
        .iter()
        .filter(|(_, _, s, _)| *s == Stage::Stage1Framework)
        .collect();
    let stage2: Vec<_> = calls
        .iter()
        .filter(|(_, _, s, _)| *s == Stage::Stage2Sketch)
        .collect();
    assert_eq!(stage1.len(), 1, "framework stage called once");
    assert_eq!(
        stage2.len(),
        4,
        "sketch stage called once per remaining sketch"
    );

    // Stage-1 honors framework_jobs; stage-2 derives per-worker jobs
    // from the host's core budget split across `sketch_jobs` workers
    // (#335). With `sketch_jobs=4` here, every stage-2 worker must see
    // the same `stage2_jobs_per_worker(4)` value the dispatcher passed.
    assert_eq!(stage1[0].3, 1, "framework_jobs=1 forwarded to stage 1");
    let expected_stage2_jobs = stage2_jobs_per_worker(4);
    for (_, _, _, jobs) in &stage2 {
        assert_eq!(
            *jobs, expected_stage2_jobs,
            "stage-2 workers must receive jobs=stage2_jobs_per_worker(sketch_jobs)"
        );
    }

    // Per-sketch output paths must be unique — the firmware_paths set
    // is built by the mock builder, which `assert!`s uniqueness on
    // insert. We also cross-check the count here as a safety net.
    let firmwares = mock.firmware_paths();
    assert_eq!(firmwares.len(), sketches.len(), "one firmware per sketch");
}

/// AC: stage-1 results are placed at index 0; stage-2 results follow
/// input order regardless of completion order.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn results_are_returned_in_input_order() {
    let tmp = tempfile::tempdir().unwrap();
    let sketches: Vec<PathBuf> = (0..6)
        .map(|i| make_sketch(tmp.path(), &format!("ordered{i}"), "uno"))
        .collect();

    let mock = MockBuilder::new();
    let result = compile_many_with(make_request(sketches.clone(), 1, 3), &mock)
        .await
        .expect("ok");

    for (i, r) in result.results.iter().enumerate() {
        assert_eq!(r.sketch, sketches[i], "result {i} should match input order");
    }
    assert_eq!(result.results[0].stage, Stage::Stage1Framework);
    for r in &result.results[1..] {
        assert_eq!(r.stage, Stage::Stage2Sketch);
    }
}

/// AC: with `sketch_jobs >= N`, all stage-2 workers run truly
/// concurrently — proven via a Barrier that would deadlock under serial
/// execution. The barrier counts each stage-2 worker as it crosses; a
/// successful test means N workers ran in parallel.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stage2_workers_run_concurrently() {
    let tmp = tempfile::tempdir().unwrap();
    let n_stage2 = 4;
    let total = n_stage2 + 1;
    let sketches: Vec<PathBuf> = (0..total)
        .map(|i| make_sketch(tmp.path(), &format!("concurrent{i}"), "uno"))
        .collect();

    // Barrier sized to exactly the stage-2 worker count. If
    // `compile_many` ran them serially, `wait()` would block forever
    // (only one worker at a time would arrive), and the test would
    // hang. We wrap in `tokio::time::timeout` so a regression is a
    // fast failure rather than a CI hang.
    let barrier = Arc::new(Barrier::new(n_stage2));
    let mock = Arc::new(MockBuilder::with_barrier(Arc::clone(&barrier)));
    let req = make_request(sketches, 1, n_stage2);

    let mock_for_task = Arc::clone(&mock);
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        compile_many_with(req, mock_for_task.as_ref()),
    )
    .await
    .unwrap_or_else(|_| {
        panic!(
            "stage-2 deadlock: barrier expected {} concurrent workers but only {} arrived",
            n_stage2,
            mock.stage2_wait_count.load(Ordering::Relaxed)
        )
    })
    .expect("compile_many");

    assert!(result.all_success);
    assert_eq!(result.stage2_count, n_stage2);
    assert_eq!(
        mock.stage2_wait_count.load(Ordering::Relaxed),
        n_stage2,
        "every stage-2 worker should have crossed the barrier"
    );
}

/// AC: stage-1 failure short-circuits stage 2 — no point fanning out
/// when the framework build is broken, every stage-2 worker would just
/// repeat the same error.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stage1_failure_skips_stage2() {
    struct FailingStage1;
    #[async_trait::async_trait]
    impl SketchBuilder for FailingStage1 {
        async fn build(&self, inputs: SketchBuildInputs) -> SketchResult {
            SketchResult {
                sketch: inputs.sketch.clone(),
                env_name: inputs.env_name,
                success: inputs.stage != Stage::Stage1Framework,
                firmware_path: None,
                elf_path: None,
                build_time_secs: 0.0,
                log_path: None,
                message: "mock failure".to_string(),
                stage: inputs.stage,
                worker_index: None,
                seed_time_secs: 0.0,
                seed_applied: false,
            }
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let sketches: Vec<PathBuf> = (0..3)
        .map(|i| make_sketch(tmp.path(), &format!("fail{i}"), "uno"))
        .collect();
    let result = compile_many_with(make_request(sketches, 1, 2), &FailingStage1)
        .await
        .expect("ok");
    assert!(!result.all_success);
    assert_eq!(result.stage1_count, 1);
    assert_eq!(
        result.stage2_count, 0,
        "stage 2 must be skipped on stage-1 failure"
    );
    assert_eq!(
        result.results.len(),
        1,
        "only the failing stage-1 sketch is returned"
    );
}

/// AC: a single sketch falls through stage 1 only — stage 2 is empty.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn single_sketch_runs_only_stage1() {
    let tmp = tempfile::tempdir().unwrap();
    let sketch = make_sketch(tmp.path(), "only", "uno");
    let mock = MockBuilder::new();
    let result = compile_many_with(make_request(vec![sketch], 2, 4), &mock)
        .await
        .expect("ok");
    assert!(result.all_success);
    assert_eq!(result.stage1_count, 1);
    assert_eq!(result.stage2_count, 0);
    assert_eq!(result.results[0].stage, Stage::Stage1Framework);
}

/// AC: stage-2 workers must keep total in-flight compile slots at roughly
/// the host's core count — never <1, never silently >cores. The previous
/// `jobs=1` hardcoding (FastLED/fbuild#335) capped each worker to a single
/// compile thread even when the orchestrator was re-building the framework
/// inside the worker, producing a ~2x slowdown vs. a single `fbuild build`
/// on a 16-core host. This locks the new core-split behavior so a future
/// regression to `jobs=1` (or to an oversubscribing default like
/// `cores` per worker) gets caught at unit-test speed instead of in CI.
#[test]
fn stage2_jobs_per_worker_splits_cores_across_workers() {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .max(1);

    // Always >= 1, never zero — the hot path multiplies this in, so a
    // bug that produced 0 would silently turn every stage-2 build into a
    // no-op.
    for sketch_jobs in [1usize, 2, 3, 4, 8, 16, 32] {
        let per = stage2_jobs_per_worker(sketch_jobs);
        assert!(
            per >= 1,
            "stage2_jobs_per_worker({sketch_jobs}) returned {per}, expected >= 1"
        );
    }

    // sketch_jobs=1 should grant the worker the whole core budget so
    // serial-batch invocations don't regress vs `fbuild build` (which
    // uses the same effective parallelism). On a single-core box this
    // still resolves to 1 — `available_parallelism()` is the floor.
    let lone_worker = stage2_jobs_per_worker(1);
    assert_eq!(
        lone_worker,
        cores.max(1),
        "with one stage-2 worker the worker should own the full core budget"
    );

    // Sum across all stage-2 workers must stay within the host's cores
    // — the whole point of splitting is to avoid oversubscription. We
    // tolerate a small undershoot when cores doesn't divide sketch_jobs
    // evenly (each worker still gets a floor of 1).
    for sketch_jobs in [2usize, 4, 8] {
        let per = stage2_jobs_per_worker(sketch_jobs);
        let total = per * sketch_jobs;
        // Floor of 1 per worker means total can exceed cores when
        // sketch_jobs > cores; bound only when sketch_jobs <= cores.
        if sketch_jobs <= cores {
            assert!(
                total <= cores,
                "{sketch_jobs} workers × {per} jobs = {total} exceeds {cores} cores"
            );
        }
    }

    // sketch_jobs > cores ⇒ each worker still gets a floor of 1 — better
    // for the worker to spawn no extra parallelism than to stall at 0.
    let many = stage2_jobs_per_worker(cores * 8);
    assert_eq!(
        many, 1,
        "oversubscribed sketch_jobs should clamp per-worker jobs to the floor of 1"
    );

    // sketch_jobs=0 must not divide-by-zero — the public API takes
    // Option<usize> but the helper takes usize, and callers in the wild
    // can hand us a literal 0 (rounding, off-by-one). Treat 0 as 1.
    assert_eq!(
        stage2_jobs_per_worker(0),
        cores.max(1),
        "stage2_jobs_per_worker(0) must not panic and should treat 0 as 1"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stage2_results_report_worker_and_seed_diagnostics() {
    let tmp = tempfile::tempdir().unwrap();
    let sketches: Vec<PathBuf> = (0..4)
        .map(|i| make_sketch(tmp.path(), &format!("diag{i}"), "uno"))
        .collect();

    let mock = MockBuilder::new();
    let mut req = make_request(sketches, 1, 2);
    req.diag_stage2 = true;
    let result = compile_many_with(req, &mock).await.expect("compile_many");

    let stage2: Vec<_> = result
        .results
        .iter()
        .filter(|r| r.stage == Stage::Stage2Sketch)
        .collect();
    assert_eq!(stage2.len(), 3);
    for r in stage2 {
        assert!(
            r.worker_index.is_some(),
            "stage-2 result should report worker index"
        );
        assert!(
            r.seed_time_secs >= 0.0,
            "stage-2 result should report seed timing"
        );
    }
}
