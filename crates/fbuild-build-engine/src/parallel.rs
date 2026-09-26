//! Parallel source file compilation.
//!
//! FastLED/fbuild#820 (Phase B of #813): converted from
//! `std::thread::scope` work-stealing to `tokio::task::JoinSet` so
//! the per-TU `Compiler::compile` futures can `.await` the embedded
//! `ZccacheService` directly, with no `Handle::block_on` and no
//! dedicated OS threads for compile dispatch.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use fbuild_core::{BuildLog, FbuildError, Result};
use tokio::sync::Semaphore;

use crate::compiler::{Compiler, CompilerBase};
use crate::flag_overlay::LanguageExtraFlags;

/// Default job count: num_cpus * 2.
pub fn default_jobs() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get() * 2)
        .unwrap_or(4)
}

/// Resolve the effective job count from an optional override.
pub fn effective_jobs(jobs: Option<usize>) -> usize {
    jobs.unwrap_or_else(default_jobs).max(1)
}

/// Result of parallel compilation.
pub struct ParallelCompileResult {
    /// Object file paths (in source order).
    pub objects: Vec<PathBuf>,
    /// Collected compiler stderr (warnings) from successful compilations.
    pub warnings: Vec<String>,
}

/// Compile source files in parallel, gating concurrency with a
/// `tokio::sync::Semaphore`.
///
/// Spawns each per-file compile as a `JoinSet` task; the semaphore
/// permits cap concurrent in-flight compiles at `jobs`. Stops on first
/// compilation error and returns object file paths (in source order)
/// plus collected warnings.
///
/// FastLED/fbuild#820 (Phase B of #813): replaces the old
/// `std::thread::scope` work-stealing loop. The borrowed `&dyn
/// Compiler` is held across `.await` points safely because
/// `compile_sources_parallel` is `async fn` and the per-task futures
/// borrow `compiler` for the duration of the JoinSet — the outer fn
/// `.await`s every task before returning, so the borrow is alive
/// throughout.
pub async fn compile_sources_parallel(
    compiler: &(dyn Compiler + Send + Sync),
    sources: &[PathBuf],
    build_dir: &Path,
    extra_flags: &LanguageExtraFlags,
    jobs: usize,
    build_log: Option<&std::sync::Mutex<BuildLog>>,
) -> Result<ParallelCompileResult> {
    let permits = jobs.min(sources.len()).max(1);
    compile_sources_parallel_shared(
        compiler,
        sources,
        build_dir,
        extra_flags,
        &Arc::new(Semaphore::new(permits)),
        build_log,
    )
    .await
}

/// [`compile_sources_parallel`] drawing permits from a caller-owned
/// semaphore, so several compile phases (framework core, variant, sketch,
/// libraries) can run at the same time without exceeding one job budget
/// (FastLED/fbuild#1468). The semaphore is FIFO, so sources submitted
/// earlier start first.
pub async fn compile_sources_parallel_shared(
    compiler: &(dyn Compiler + Send + Sync),
    sources: &[PathBuf],
    build_dir: &Path,
    extra_flags: &LanguageExtraFlags,
    semaphore: &Arc<Semaphore>,
    build_log: Option<&std::sync::Mutex<BuildLog>>,
) -> Result<ParallelCompileResult> {
    // Build work list: (source, object) pairs needing rebuild
    let mut work: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut objects: Vec<PathBuf> = Vec::new();

    for source in sources {
        let obj = CompilerBase::object_path(source, build_dir);
        let source_flags = extra_flags.for_source(source);
        // The object path anchors the workspace: a `.cmdhash` written by a
        // sibling workspace with an identical effective compile command must
        // match this check (stage-2 seeding, FastLED/fbuild#1346).
        let signature = compiler.rebuild_signature(source, &source_flags, &obj);
        if CompilerBase::needs_rebuild_with_signature(source, &obj, Some(&signature)) {
            work.push((source.clone(), obj.clone()));
        }
        objects.push(obj);
    }

    if work.is_empty() {
        return Ok(ParallelCompileResult {
            objects,
            warnings: Vec::new(),
        });
    }

    let total = work.len();
    tracing::info!(
        "compiling {} files with up to {} concurrent tasks",
        total,
        semaphore.available_permits().min(total)
    );

    let compiled_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut warnings: Vec<String> = Vec::new();
    let mut first_error: Option<String> = None;

    // `JoinSet<Result<...>>` lets us cancel pending tasks the moment
    // the first error appears. We accumulate Result outcomes and bail
    // after draining.
    let mut tasks: tokio::task::JoinSet<std::result::Result<Option<String>, String>> =
        tokio::task::JoinSet::new();

    // SAFETY: we extend the borrow of `compiler` / `extra_flags` /
    // `build_log` to `'static` for the duration of the JoinSet's
    // lifetime. The outer `async fn` awaits every spawned task before
    // returning, so the borrows are alive for as long as the tasks
    // execute. `transmute` is the standard idiom for this scoped-task
    // pattern in tokio (no scoped tasks in tokio today).
    let compiler_ptr: &'static (dyn Compiler + Send + Sync) =
        unsafe { std::mem::transmute(compiler) };
    let extra_flags_ptr: &'static LanguageExtraFlags = unsafe { std::mem::transmute(extra_flags) };
    let build_log_ptr: Option<&'static std::sync::Mutex<BuildLog>> =
        unsafe { std::mem::transmute(build_log) };

    for (source, obj) in work.into_iter() {
        let sem = semaphore.clone();
        let counter = compiled_count.clone();
        tasks.spawn(async move {
            // Acquire permit; if Acquired returns Err, semaphore was closed
            // (only happens on shutdown — propagate as immediate-error).
            let _permit = sem
                .acquire()
                .await
                .map_err(|e| format!("semaphore closed: {e}"))?;

            let source_flags = extra_flags_ptr.for_source(&source);
            match compiler_ptr.compile(&source, &obj, &source_flags).await {
                Ok(result) if result.success => {
                    let stderr = result.stderr.trim().to_string();
                    let count = counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                    if count.is_multiple_of(20) || count == total {
                        tracing::info!("[{}/{}] compiled", count, total);
                        if let Some(log) = build_log_ptr {
                            if let Ok(mut log) = log.lock() {
                                log.push(format!("Compiled {}/{} files", count, total));
                            }
                        }
                    }
                    if stderr.is_empty() {
                        Ok(None)
                    } else {
                        Ok(Some(stderr))
                    }
                }
                Ok(result) => Err(format!(
                    "compilation failed for {}:\n{}",
                    source.display(),
                    result.stderr
                )),
                Err(e) => Err(e.to_string()),
            }
        });
    }

    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok(Ok(Some(warning))) => warnings.push(warning),
            Ok(Ok(None)) => {}
            Ok(Err(msg)) => {
                if first_error.is_none() {
                    first_error = Some(msg);
                    // Abort remaining tasks; we already have an error.
                    tasks.abort_all();
                }
            }
            Err(join_err) => {
                if first_error.is_none() {
                    first_error = Some(format!(
                        "compile task panicked or was cancelled: {join_err}"
                    ));
                    tasks.abort_all();
                }
            }
        }
    }

    if let Some(error) = first_error {
        return Err(FbuildError::BuildFailed(error));
    }

    Ok(ParallelCompileResult { objects, warnings })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_jobs() {
        let jobs = default_jobs();
        assert!(jobs >= 2, "should be at least 2 (1 cpu * 2)");
    }

    #[test]
    fn test_effective_jobs_with_override() {
        assert_eq!(effective_jobs(Some(8)), 8);
    }

    #[test]
    fn test_effective_jobs_minimum() {
        assert_eq!(effective_jobs(Some(0)), 1);
    }

    #[test]
    fn test_effective_jobs_default() {
        let jobs = effective_jobs(None);
        assert!(jobs >= 2);
    }

    /// Records in-flight compiles and each compile's start/end instants.
    struct TracingCompiler {
        gcc: PathBuf,
        in_flight: std::sync::atomic::AtomicUsize,
        max_in_flight: std::sync::atomic::AtomicUsize,
        spans: std::sync::Mutex<Vec<(PathBuf, std::time::Instant, std::time::Instant)>>,
    }

    #[async_trait::async_trait]
    impl Compiler for TracingCompiler {
        async fn compile_one(
            &self,
            _compiler_path: &Path,
            source: &Path,
            output: &Path,
            _flags: &[String],
            _extra_flags: &[String],
        ) -> Result<crate::compiler::CompileResult> {
            use std::sync::atomic::Ordering;
            let started = std::time::Instant::now();
            let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_in_flight.fetch_max(now, Ordering::SeqCst);
            // Decrements even if the task is aborted mid-sleep.
            let _in_flight = InFlight(&self.in_flight);
            let fail = source.to_string_lossy().contains("bad");
            let delay = if fail { 5 } else { 40 };
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            if fail {
                return Err(FbuildError::BuildFailed("synthetic failure".into()));
            }
            self.spans.lock().unwrap().push((
                source.to_path_buf(),
                started,
                std::time::Instant::now(),
            ));
            Ok(crate::compiler::CompileResult {
                success: true,
                object_file: output.to_path_buf(),
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 0,
            })
        }

        fn gcc_path(&self) -> &Path {
            &self.gcc
        }

        fn gxx_path(&self) -> &Path {
            &self.gcc
        }

        fn c_flags(&self) -> Vec<String> {
            Vec::new()
        }

        fn cpp_flags(&self) -> Vec<String> {
            Vec::new()
        }

        fn rebuild_signature(&self, source: &Path, _extra: &[String], _out: &Path) -> String {
            source.to_string_lossy().into_owned()
        }
    }

    struct InFlight<'a>(&'a std::sync::atomic::AtomicUsize);

    impl Drop for InFlight<'_> {
        fn drop(&mut self) {
            self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// FastLED/fbuild#1468: the pipeline runs compile phases concurrently
    /// with borrowed state that `compile_sources_parallel_shared` extends to
    /// `'static`. That is only sound if a failing call still returns only
    /// after none of its tasks can run, so a failure must not leave compiles
    /// in flight.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_failing_compile_returns_only_after_its_tasks_stop() {
        let tmp = tempfile::tempdir().unwrap();
        let sources: Vec<PathBuf> = ["bad.c", "ok0.c", "ok1.c", "ok2.c"]
            .iter()
            .map(|name| {
                let path = tmp.path().join(name);
                std::fs::write(&path, "int x;\n").unwrap();
                path
            })
            .collect();
        let compiler = TracingCompiler {
            gcc: PathBuf::from("/toolchain/bin/gcc"),
            in_flight: Default::default(),
            max_in_flight: Default::default(),
            spans: Default::default(),
        };
        let slots = Arc::new(Semaphore::new(4));
        let result = compile_sources_parallel_shared(
            &compiler,
            &sources,
            &tmp.path().join("obj"),
            &LanguageExtraFlags::default(),
            &slots,
            None,
        )
        .await;
        assert!(result.is_err());
        assert_eq!(
            compiler.in_flight.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "no compile may still be running once the call returns"
        );
        assert_eq!(slots.available_permits(), 4, "every permit is released");
    }

    /// FastLED/fbuild#1468: compile phases sharing one semaphore overlap
    /// (the second phase fills permits the first phase's tail leaves idle)
    /// without ever exceeding the shared job budget.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn phases_sharing_a_semaphore_overlap_within_the_budget() {
        let tmp = tempfile::tempdir().unwrap();
        let sources = |phase: &str| -> Vec<PathBuf> {
            (0..3)
                .map(|i| {
                    let path = tmp.path().join(format!("{phase}{i}.c"));
                    std::fs::write(&path, "int x;\n").unwrap();
                    path
                })
                .collect()
        };
        let (core, sketch) = (sources("core"), sources("sketch"));
        let compiler = TracingCompiler {
            gcc: PathBuf::from("/toolchain/bin/gcc"),
            in_flight: Default::default(),
            max_in_flight: Default::default(),
            spans: Default::default(),
        };
        let slots = Arc::new(Semaphore::new(2));
        let flags = LanguageExtraFlags::default();
        let (core_build, sketch_build) = (tmp.path().join("core"), tmp.path().join("src"));

        let (a, b) = tokio::join!(
            compile_sources_parallel_shared(&compiler, &core, &core_build, &flags, &slots, None),
            compile_sources_parallel_shared(
                &compiler,
                &sketch,
                &sketch_build,
                &flags,
                &slots,
                None
            ),
        );
        assert_eq!(a.unwrap().objects.len(), 3);
        assert_eq!(b.unwrap().objects.len(), 3);

        assert_eq!(
            compiler
                .max_in_flight
                .load(std::sync::atomic::Ordering::SeqCst),
            2,
            "the shared budget of 2 permits bounds both phases together"
        );
        let spans = compiler.spans.lock().unwrap();
        let is_core = |path: &Path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("core")
        };
        let last_core_end = spans
            .iter()
            .filter(|s| is_core(&s.0))
            .map(|s| s.2)
            .max()
            .unwrap();
        let first_sketch_start = spans
            .iter()
            .filter(|s| !is_core(&s.0))
            .map(|s| s.1)
            .min()
            .unwrap();
        assert!(
            first_sketch_start < last_core_end,
            "the sketch phase must start while core compiles are still running"
        );
    }
}
