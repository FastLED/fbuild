//! The ESP32 build's single compile job pool (FastLED/fbuild#1559).
//!
//! Everything is resolved first: `lib_deps` (sources only), library
//! selection, framework-library cache hits, the project-as-library and local
//! `lib/` libraries. Then every compile runs in one `tokio::join!`, each
//! translation unit drawing a permit from one [`JobGate`]: `lib_deps`,
//! framework libraries, project-as-library, core + sketch and local
//! libraries, with the boot artifacts (not gated) alongside. Only the link
//! waits for all of it.
//!
//! [`link_order`] assembles the library archives in exactly the order the
//! former serial phases produced them, so the link line is unchanged.

use std::collections::HashSet;
use std::future::Future;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use fbuild_core::{BuildLog, Result};
use fbuild_packages::library::library_compiler::JobGate;

use super::compile_phases::{CompileTarget, compile_core_and_sketch};
use super::framework_libs::{FwLibsCompiled, FwLibsPlan};
use crate::compiler::Compiler;
use crate::parallel::ParallelCompileResult;
use crate::perf_log::PerfTimer;

/// Framework-library names that a `lib_deps` or project-as-library archive
/// already provides, so the framework copy is skipped.
///
/// Mirrors the former check against the file stems of the archives built
/// before the framework libraries (`lib{name}.a`, name case preserved): the
/// `lib_deps` archives and the project-as-library. Local `lib/` libraries
/// were built afterwards and never took part.
pub(super) fn already_compiled(
    lib_deps: Vec<String>,
    project_library: Option<String>,
) -> HashSet<String> {
    lib_deps.into_iter().chain(project_library).collect()
}

/// Library archives in link order: `lib_deps`, the project-as-library,
/// framework libraries (selection order), then local `lib/` libraries.
pub(super) fn link_order<A>(
    lib_deps: Vec<A>,
    project_library: Option<A>,
    framework_libs: Vec<A>,
    local_libs: Vec<A>,
) -> Vec<A> {
    lib_deps
        .into_iter()
        .chain(project_library)
        .chain(framework_libs)
        .chain(local_libs)
        .collect()
}

/// The library compiles that join the pool, as futures already bound to the
/// pool's gate. Framework libraries and core + sketch are run by [`run`].
pub(super) struct LibraryJobs<LD, PL, LL, B> {
    /// `lib_deps` archives in library order.
    pub lib_deps: LD,
    /// The project-as-library archive, if any.
    pub project_library: PL,
    /// Local `lib/` archives in library order.
    pub local_libs: LL,
    /// Boot artifacts; not a compile, so not gated. Yields its duration.
    pub boot_artifacts: B,
}

/// Everything the pool compiled; see [`link_order`] for the archives.
pub(super) struct PoolOutput<A> {
    pub lib_deps: Vec<A>,
    pub project_library: Option<A>,
    pub framework_libs: FwLibsCompiled,
    pub core: ParallelCompileResult,
    pub sketch: ParallelCompileResult,
    pub local_libs: Vec<A>,
}

/// Run every compile of the build concurrently on `gate`.
///
/// Uses `tokio::join!`, never `try_join!`: `compile_sources_parallel_shared`
/// extends its borrows to `'static` and is sound only when every branch is
/// awaited to completion. Errors are propagated afterwards, in the order the
/// former serial phases would have hit them. Framework-library failures are
/// not errors; [`FwLibsPlan::finish`] records them.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run<S, T, A, LD, PL, LL, B>(
    perf: &mut PerfTimer,
    gate: &JobGate,
    compiler: &(dyn Compiler + Send + Sync),
    core: CompileTarget<'_, S>,
    sketch: CompileTarget<'_, T>,
    build_log: &Mutex<BuildLog>,
    framework_libs: &FwLibsPlan,
    jobs: LibraryJobs<LD, PL, LL, B>,
) -> Result<PoolOutput<A>>
where
    S: AsRef<Path> + Send + Sync,
    T: AsRef<Path> + Send + Sync,
    LD: Future<Output = Result<Vec<A>>>,
    PL: Future<Output = Result<Option<A>>>,
    LL: Future<Output = Result<Vec<A>>>,
    B: Future<Output = Result<Duration>>,
{
    let lib_deps = timed(jobs.lib_deps);
    let project_library = timed(jobs.project_library);
    let local_libs = timed(jobs.local_libs);
    let fw = framework_libs.compile(gate);
    // Sketch first inside, so its single TU starts with the first wave.
    let core_sketch = compile_core_and_sketch(compiler, perf, gate, core, sketch, build_log);
    let (compiled, fw, (lib_deps, lib_deps_elapsed), project_library, local_libs, boot) = tokio::join!(
        core_sketch,
        fw,
        lib_deps,
        project_library,
        local_libs,
        jobs.boot_artifacts
    );
    perf.record("lib-deps", lib_deps_elapsed);
    perf.record("project-lib", project_library.1);
    perf.record("compile-local-libs", local_libs.1);

    let lib_deps = lib_deps?;
    let project_library = project_library.0?;
    let (core, sketch) = compiled?;
    perf.record("boot-artifacts", boot?);
    let local_libs = local_libs.0?;
    Ok(PoolOutput {
        lib_deps,
        project_library,
        framework_libs: fw,
        core,
        sketch,
        local_libs,
    })
}

/// `fut`'s output with its own wall time, so overlapping branches each report
/// how long they took rather than the whole pool's span.
async fn timed<F: Future>(fut: F) -> (F::Output, Duration) {
    let started = Instant::now();
    let output = fut.await;
    (output, started.elapsed())
}

#[cfg(test)]
#[path = "job_pool_tests.rs"]
mod tests;
