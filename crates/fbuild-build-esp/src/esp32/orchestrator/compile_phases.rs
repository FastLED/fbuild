//! Concurrent compile phases for the ESP32 orchestrator.
//!
//! The framework core and the sketch are independent source sets that link
//! together, so nothing about one depends on the other having finished. They
//! are compiled against a single shared job gate here rather than one after
//! the other, which is what this module exists to hold (FastLED/fbuild#1537).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use fbuild_core::{BuildLog, Result};
use tokio::sync::Semaphore;

use crate::compiler::Compiler;
use crate::flag_overlay::LanguageExtraFlags;
use crate::parallel::{ParallelCompileResult, compile_sources_parallel_shared};
use crate::perf_log::PerfTimer;

/// Compile the framework core sources and the sketch sources concurrently.
///
/// Returns `(core, sketch)`. The sketch is submitted first and the gate has
/// far more permits than the first wave needs, so its single translation unit
/// starts alongside the core fan-out instead of being scheduled after all of
/// it. Sharing one gate also keeps the region at `jobs` compilers rather than
/// letting each phase hold its own full pool.
///
/// One [`PerfTimer::phase`] guard covers the region both phases share, so
/// `compile-core-variant` spans it; the sketch's own span is recorded
/// separately, so the two entries deliberately overlap.
pub(super) async fn compile_core_and_sketch(
    compiler: &(dyn Compiler + Send + Sync),
    perf: &mut PerfTimer,
    jobs: usize,
    core: CompileTarget<'_>,
    sketch: CompileTarget<'_>,
    build_log: &Mutex<BuildLog>,
) -> Result<(ParallelCompileResult, ParallelCompileResult)> {
    let gate = Arc::new(Semaphore::new(jobs.max(1)));
    let sketch_started = Instant::now();

    let sketch_fut = async {
        let result = compile_sources_parallel_shared(
            compiler,
            sketch.sources,
            sketch.build_dir,
            sketch.overlay,
            &gate,
            Some(build_log),
        )
        .await;
        (result, sketch_started.elapsed())
    };
    let core_fut = async {
        compile_sources_parallel_shared(
            compiler,
            core.sources,
            core.build_dir,
            core.overlay,
            &gate,
            Some(build_log),
        )
        .await
    };

    let ((sketch_result, sketch_elapsed), core_result) = {
        let _region = perf.phase("compile-core-variant");
        tokio::join!(sketch_fut, core_fut)
    };
    perf.record("compile-sketch", sketch_elapsed);

    Ok((core_result?, sketch_result?))
}

/// One source set to compile, with the build dir and flag overlay it needs.
pub(super) struct CompileTarget<'a> {
    /// Sources to compile.
    pub sources: &'a [PathBuf],
    /// Directory the objects are written to.
    pub build_dir: &'a Path,
    /// Per-language extra flags for this source set.
    pub overlay: &'a LanguageExtraFlags,
}
