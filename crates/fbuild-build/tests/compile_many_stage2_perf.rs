//! Real-toolchain regression gate for the stage-2 framework-archive
//! sharing fix (FastLED/fbuild#335 / PR #337). Empirically, with the seed
//! in place, each stage-2 sketch's `build_time_secs` should be a small
//! fraction of stage-1's — because stage 2 reuses stage 1's compiled
//! framework `core/` via the per-worker seed step in `run_stage2`.
//!
//! This test scaffolds 4 identical blink sketches, runs `compile_many`
//! cold (no shared cache other than the seed), and asserts that every
//! stage-2 sketch landed under a tight multiple of stage-1's wall time.
//! A regression that re-introduces per-stage-2 framework rebuilds would
//! push stage-2 times up to roughly stage-1 wall and fail the assertion.
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

use fbuild_build::compile_many::{CompileManyRequest, Stage, compile_many};
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

    let stage1_secs = stage1.build_time_secs;
    eprintln!(
        "stage 1 wall: {:.2}s  ({})",
        stage1_secs,
        stage1.sketch.display()
    );

    // Threshold: stage-2 must come in well under stage-1. If the seed
    // is doing its job, every framework `compiler.compile` call is a
    // zccache hit (or an mtime-fresh skip via the seed) and stage 2's
    // remaining work is sketch.cpp + link + size — which empirically
    // lands around 200ms vs stage-1's ~600ms-1.5s on the same hardware.
    //
    // The bound is intentionally loose (50%) so this passes on slow CI
    // runners and tightens enough to catch a regression that puts
    // stage-2 back at "rebuild the framework from scratch" (which
    // would be ≥80% of stage-1).
    let max_allowed = stage1_secs * 0.5;
    for r in &stage2 {
        eprintln!(
            "stage 2 wall: {:.2}s  ({})  [must be < {:.2}s]",
            r.build_time_secs,
            r.sketch.display(),
            max_allowed
        );
        assert!(
            r.build_time_secs < max_allowed,
            "stage-2 sketch {} wall {:.2}s exceeded 50% of stage-1 \
             ({:.2}s); the framework-archive seed (FastLED/fbuild#337) \
             is likely not actually skipping the recompile. Inspect \
             {}/.fbuild/build/uno/release/compile_many.log — if it \
             prints `Compiled 25/25 files` followed by `Linking firmware.elf` \
             but the .o mtimes match stage-1's, the per-file zccache hit \
             is succeeding and the wall regression is elsewhere; otherwise \
             check that `seed_stage2_core_from_stage1` ran (look for the \
             `compile-many stage 2 seed: linked N + copied N` tracing \
             info line at info level).",
            r.sketch.display(),
            r.build_time_secs,
            stage1_secs,
            r.sketch.display(),
        );
    }
}
