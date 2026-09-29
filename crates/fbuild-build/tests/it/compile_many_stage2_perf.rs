//! Real-toolchain regression gate for the stage-2 framework-archive
//! sharing fix (FastLED/fbuild#335 / PR #337). Stage 2 reuses stage 1's
//! compiled framework `core/` via the per-worker seed step in `run_stage2`;
//! the orchestrator's freshness check must then skip every framework TU.
//!
//! This test scaffolds 4 identical blink sketches and runs `compile_many`.
//! The regression signal is **work done**, read from each stage-2 sketch's
//! `compile_many.log` (`Compiled N/M files` lines): with the seed working,
//! a stage-2 worker compiles only its own sketch translation unit (M == 1
//! for this fixture). A regression that re-introduces per-stage-2 framework
//! rebuilds — e.g. FastLED/fbuild#1346, where seeded `.cmdhash` files never
//! matched because sibling workspaces hashed their workspace-relative
//! include flags differently — shows up as M == 26 here.
//!
//! Wall times are printed for diagnostics but not asserted: since the
//! global core-artifact cache can pre-hydrate stage 1, both stages' fixed
//! link/size costs dominate and wall ratios are machine-load noise.
//!
//! Gated `#[ignore]` because it downloads avr-gcc + Arduino-AVR core on
//! the first run (cached afterward). Run with:
//!
//! ```bash
//! soldr cargo test -p fbuild-build --test compile_many_stage2_perf \
//!   -- --ignored
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use fbuild_build::compile_backend;
use fbuild_build::compile_many::{CompileManyRequest, SketchResult, Stage, compile_many};
use fbuild_core::BuildProfile;

/// 15-min wall-clock cap for `--ignored` real-toolchain tests (FastLED/fbuild#806).
const REAL_BUILD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(900);

async fn under_test_timeout<F: std::future::Future>(fut: F) -> F::Output {
    match tokio::time::timeout(REAL_BUILD_TIMEOUT, fut).await {
        Ok(v) => v,
        Err(_) => panic!(
            "real-toolchain test exceeded {:.0}s budget — see FastLED/fbuild#806",
            REAL_BUILD_TIMEOUT.as_secs_f64()
        ),
    }
}

/// `compile_many` compiles through the process-wide compile backend
/// (FastLED/fbuild#800), which only the daemon wires at startup — this
/// integration test must install its own.
async fn install_test_compile_backend() {
    static INSTALL: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    INSTALL
        .get_or_init(|| async {
            let backend = compile_backend::CompileBackend::start()
                .await
                .expect("compile backend starts for stage-2 perf test");
            compile_backend::install_global(backend);
        })
        .await;
}

const UNO_PLATFORMIO_INI: &str =
    "[env:uno]\nplatform = atmelavr\nboard = uno\nframework = arduino\n";

const UNO_BLINK_INO: &str = "\
void setup() {
  pinMode(13, OUTPUT);
}

void loop() {
  digitalWrite(13, HIGH);
  delay(1000);
  digitalWrite(13, LOW);
  delay(1000);
}
";

fn scaffold_uno_blink(project_dir: &Path) {
    fs::write(project_dir.join("platformio.ini"), UNO_PLATFORMIO_INI).unwrap();
    let src_dir = project_dir.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    fs::write(src_dir.join("blink.ino"), UNO_BLINK_INO).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "downloads AVR toolchain + measures wall-time; perf oracle, flaky under CI load"]
async fn stage2_per_sketch_wall_is_a_fraction_of_stage1() {
    install_test_compile_backend().await;
    let tmp = tempfile::TempDir::new().unwrap();
    let sketches: Vec<PathBuf> = (0..4)
        .map(|i| {
            let p = tmp.path().join(format!("s{i}"));
            fs::create_dir_all(&p).unwrap();
            scaffold_uno_blink(&p);
            p
        })
        .collect();

    let req = CompileManyRequest {
        board: "uno".to_string(),
        sketches: sketches.clone(),
        framework_jobs: Some(2),
        sketch_jobs: Some(4),
        profile: BuildProfile::Release,
        verbose: false,
        pio_env: Default::default(),
        diag_stage2: true,
    };

    let result = under_test_timeout(compile_many(req))
        .await
        .expect("compile_many should not error");
    assert!(
        result.all_success,
        "every sketch should build: results={:?}",
        result
            .results
            .iter()
            .map(|r| (r.sketch.clone(), r.success, r.message.clone()))
            .collect::<Vec<_>>()
    );
    assert_eq!(result.stage1_count, 1);
    assert_eq!(result.stage2_count, 3);

    let stage1 = result
        .results
        .iter()
        .find(|r| r.stage == Stage::Stage1Framework)
        .expect("must have a stage-1 result");
    let stage2: Vec<_> = result
        .results
        .iter()
        .filter(|r| r.stage == Stage::Stage2Sketch)
        .collect();
    assert_eq!(stage2.len(), 3, "three stage-2 results expected");

    eprintln!(
        "stage 1 wall: {:.2}s  ({})",
        stage1.build_time_secs,
        stage1.sketch.display()
    );

    // Primary oracle — work done, not wall time. Every stage-2 worker must
    // compile only its own sketch TU against the seeded framework core. The
    // fixture's blink sketch is a single translation unit, so every
    // `Compiled N/M files` line in the build log must have M <= 1. A value
    // of M == 26 means the whole framework recompiled despite a successful
    // seed — the FastLED/fbuild#1346 failure signature.
    for r in &stage2 {
        eprintln!(
            "stage 2 wall: {:.2}s  ({})  seed_applied={} seed_time={:.3}s worker={:?}",
            r.build_time_secs,
            r.sketch.display(),
            r.seed_applied,
            r.seed_time_secs,
            r.worker_index
        );
        assert!(
            r.seed_applied,
            "stage-2 sketch {} should have had a core seed applied
{}",
            r.sketch.display(),
            stage2_failure_detail(r)
        );
        assert!(
            max_compiled_batch_size(r) <= 1,
            "{}",
            stage2_failure_detail(r)
        );
    }
}

/// Largest work-list size M from `Compiled N/M files` lines in this
/// sketch's `compile_many.log`. M is the number of TUs the freshness
/// check queued — with the seed working it equals the sketch's own TU
/// count.
///
/// Fails closed: a missing, unreadable, empty, or malformed log yields
/// `usize::MAX`, never 0. A log with no parsable work record proves
/// nothing about the stage-2 work limit, and returning 0 would let the
/// `<= 1` assertion pass vacuously — turning the #1346 regression oracle
/// into a no-op the moment the log format drifts. The caller surfaces
/// the log itself through [`stage2_failure_detail`].
fn max_compiled_batch_size(r: &SketchResult) -> usize {
    let Some(log_path) = r.log_path.as_deref() else {
        return usize::MAX;
    };
    let Ok(log) = fs::read_to_string(log_path) else {
        return usize::MAX;
    };
    max_compiled_batch_size_in(&log)
}

/// The parse half of [`max_compiled_batch_size`], split out so the
/// fail-closed contract is covered by a cheap unit test instead of only by
/// the `#[ignore]`d real-toolchain oracle.
fn max_compiled_batch_size_in(log: &str) -> usize {
    log.lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("Compiled ")?;
            let (_n, m) = rest.split_once('/')?;
            let m = m.split_whitespace().next()?;
            m.parse::<usize>().ok()
        })
        .max()
        .unwrap_or(usize::MAX)
}

/// The oracle is only worth having if it cannot pass by accident. These
/// lock the fail-closed contract: every shape of log that fails to prove
/// how much work stage 2 did must read as `usize::MAX`, so the `<= 1`
/// assertion rejects it — FastLED/fbuild#1346.
#[test]
fn unparsable_logs_fail_closed() {
    assert_eq!(max_compiled_batch_size_in(""), usize::MAX, "empty log");
    assert_eq!(
        max_compiled_batch_size_in(
            "Building sketch
Linking firmware.elf
"
        ),
        usize::MAX,
        "log with no work record"
    );
    assert_eq!(
        max_compiled_batch_size_in(
            "Compiled some/of files
"
        ),
        usize::MAX,
        "work record with a non-numeric denominator"
    );
    assert_eq!(
        max_compiled_batch_size_in(
            "Compiled 1 file
"
        ),
        usize::MAX,
        "work record without the N/M separator"
    );
}

/// The pass state (`M == 1`) and the #1346 failure signature
/// (`M == 26`) must both survive the parse, and multiple records take
/// the maximum rather than the last.
#[test]
fn work_records_parse_to_their_denominator() {
    assert_eq!(
        max_compiled_batch_size_in(
            "Compiled 1/1 files
"
        ),
        1
    );
    assert_eq!(
        max_compiled_batch_size_in(
            "Compiled 26/26 files
"
        ),
        26
    );
    assert_eq!(
        max_compiled_batch_size_in(
            "Compiled 1/1 files
Compiled 5/26 files
"
        ),
        26,
        "a later small batch must not mask an earlier full rebuild"
    );
}

/// Build the panic message for a failed stage-2 work assertion. Reads the
/// sketch's `compile_many.log` (still alive inside the TempDir at this
/// point) and embeds its head and tail so the failure is diagnosable after
/// the TempDir is dropped — FastLED/fbuild#1346.
fn stage2_failure_detail(r: &SketchResult) -> String {
    let log_path = r.log_path.clone().unwrap_or_else(|| {
        r.sketch.join(format!(
            "{}/{}/uno/release/compile_many.log",
            fbuild_paths::FBUILD_DIR_NAME,
            fbuild_paths::BUILD_DIR_NAME
        ))
    });
    let log = fs::read_to_string(&log_path).unwrap_or_else(|e| {
        format!(
            "<compile_many.log unreadable at {}: {e}>",
            log_path.display()
        )
    });
    let total_lines = log.lines().count();
    let head: Vec<&str> = log.lines().take(15).collect();
    let mut tail: Vec<&str> = log.lines().rev().take(40).collect::<Vec<_>>();
    tail.reverse();
    format!(
        "stage-2 sketch {} compiled more than its own sketch TU against the \
         seeded framework — the framework-archive seed (FastLED/fbuild#337) \
         is likely not actually skipping the recompile — \
         FastLED/fbuild#1346.\n\
         seed_applied={} seed_time={:.3}s worker={:?}\n\
         compile_many.log at {} ({} lines) head:\n{}\n…tail:\n{}",
        r.sketch.display(),
        r.seed_applied,
        r.seed_time_secs,
        r.worker_index,
        log_path.display(),
        total_lines,
        head.join("\n"),
        tail.join("\n"),
    )
}
