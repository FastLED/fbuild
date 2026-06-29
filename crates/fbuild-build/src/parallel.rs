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
    // Build work list: (source, object) pairs needing rebuild
    let mut work: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut objects: Vec<PathBuf> = Vec::new();

    for source in sources {
        let obj = CompilerBase::object_path(source, build_dir);
        let source_flags = extra_flags.for_source(source);
        let signature = compiler.rebuild_signature(source, &source_flags);
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
    let parallelism = jobs.min(total).max(1);
    tracing::info!(
        "compiling {} files with {} concurrent tasks",
        total,
        parallelism
    );

    let semaphore = Arc::new(Semaphore::new(parallelism));
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
                    let count =
                        counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                    if count % 20 == 0 || count == total {
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
                    first_error = Some(format!("compile task panicked or was cancelled: {join_err}"));
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
}
