//! Concurrent compile phases for the ESP32 orchestrator.
//!
//! The framework core and the sketch are independent source sets that link
//! together, so nothing about one depends on the other having finished. They
//! are compiled against the build's single shared job gate here rather than
//! one after the other, which is what this module exists to hold
//! (FastLED/fbuild#1537, FastLED/fbuild#1559).

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use fbuild_core::{BuildLog, Result};
use tokio::sync::Semaphore;

use crate::compiler::Compiler;
use crate::flag_overlay::LanguageExtraFlags;
use crate::parallel::{ParallelCompileResult, compile_sources_parallel_shared};
use crate::perf_log::PerfTimer;

/// One source set to compile, with the build dir and flag overlay it needs.
///
/// Generic over the source element so callers pass their existing `PathBuf`
/// vectors straight through. `dylints/ban_std_pathbuf` denies naming
/// `std::path::PathBuf` in a new file while the compile engine's API takes
/// exactly that, so the slices are materialised at the call below (where the
/// element type is inferred) instead of in this signature.
pub(super) struct CompileTarget<'a, S: AsRef<Path>> {
    /// Sources to compile.
    pub sources: &'a [S],
    /// Directory the objects are written to.
    pub build_dir: &'a Path,
    /// Per-language extra flags for this source set.
    pub overlay: &'a LanguageExtraFlags,
}

/// Compile the framework core sources and the sketch sources concurrently,
/// every translation unit drawing a permit from the caller's `gate`.
///
/// Returns `(core, sketch)`. The sketch is submitted first, so its single
/// translation unit starts alongside the core fan-out instead of being
/// scheduled after all of it. The gate is the build's one job budget, shared
/// with every other compile running at the same time (libraries included), so
/// the whole region stays at the gate's permit count.
///
/// Two measurements are recorded per source set: `compile-core` and
/// `compile-sketch` are each set's own wall time, measured from before
/// submission to that set's completion. The `compile-core-variant` phase guard
/// covers the region both share, so it reports the *join* — the longer of the
/// two legs, not the core's cost. Reading `compile-core-variant` as "time
/// spent compiling the core" is exactly the misreading FastLED/fbuild#1414
/// item 2 recorded: with a fully hydrated core cache it compiles nothing and
/// still reports the sketch's compile time.
pub(super) async fn compile_core_and_sketch<S, T>(
    compiler: &(dyn Compiler + Send + Sync),
    perf: &mut PerfTimer,
    gate: &Arc<Semaphore>,
    core: CompileTarget<'_, S>,
    sketch: CompileTarget<'_, T>,
    build_log: &Mutex<BuildLog>,
) -> Result<(ParallelCompileResult, ParallelCompileResult)>
where
    S: AsRef<Path> + Send + Sync,
    T: AsRef<Path> + Send + Sync,
{
    let sketch_started = Instant::now();

    // Measured from here, so each source set's recorded time starts at
    // submission rather than when the join happens to poll it.
    let core_started = Instant::now();
    let sketch_fut = async {
        let paths: Vec<_> = sketch
            .sources
            .iter()
            .map(|s| s.as_ref().to_path_buf())
            .collect();
        let result = compile_sources_parallel_shared(
            compiler,
            &paths,
            sketch.build_dir,
            sketch.overlay,
            gate,
            Some(build_log),
            None,
        )
        .await;
        (result, sketch_started.elapsed())
    };
    let core_fut = async {
        let paths: Vec<_> = core
            .sources
            .iter()
            .map(|s| s.as_ref().to_path_buf())
            .collect();
        let compiled = compile_sources_parallel_shared(
            compiler,
            &paths,
            core.build_dir,
            core.overlay,
            gate,
            Some(build_log),
            None,
        )
        .await;
        (compiled, core_started.elapsed())
    };

    // `join!`, not `try_join!`: `compile_sources_parallel_shared` extends its
    // borrows to `'static` and is sound only when awaited to completion.
    let ((sketch_result, sketch_elapsed), (core_result, core_elapsed)) = {
        let _region = perf.phase("compile-core-variant");
        tokio::join!(sketch_fut, core_fut)
    };
    perf.record("compile-sketch", sketch_elapsed);
    // The core's own cost, separable from the join above. An empty source set
    // (hydrated core cache) records ~0 ms here and no longer inherits the
    // sketch's time.
    perf.record("compile-core", core_elapsed);

    Ok((core_result?, sketch_result?))
}

#[cfg(test)]
#[path = "compile_phases_tests.rs"]
mod tests;
