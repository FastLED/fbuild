//! Parallel source file compilation.
//!
//! Uses `std::thread::scope` with a work-stealing pattern to compile
//! source files across N threads. No external dependencies needed.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use fbuild_core::{BuildLog, FbuildError, Result};

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

/// Compile source files in parallel using a thread pool.
///
/// Spawns up to `jobs` threads, each pulling work from a shared queue.
/// Stops on first compilation error.
/// Returns object file paths (in source order) and collected warnings.
///
/// If `build_log` is provided, progress milestones are streamed to it.
pub fn compile_sources_parallel(
    compiler: &dyn Compiler,
    sources: &[PathBuf],
    build_dir: &Path,
    extra_flags: &LanguageExtraFlags,
    jobs: usize,
    build_log: Option<&Mutex<BuildLog>>,
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
    let thread_count = jobs.min(total);
    tracing::info!("compiling {} files with {} threads", total, thread_count);

    let work_iter = Mutex::new(work.into_iter());
    let first_error: Mutex<Option<String>> = Mutex::new(None);
    let compiled_count = AtomicUsize::new(0);
    let all_warnings: Mutex<Vec<String>> = Mutex::new(Vec::new());

    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..thread_count)
            .map(|_| {
                scope.spawn(|| {
                    let mut local_warnings: Vec<String> = Vec::new();
                    loop {
                        // Check for early termination
                        if first_error.lock().unwrap().is_some() {
                            break;
                        }

                        let job = work_iter.lock().unwrap().next();
                        let (source, obj) = match job {
                            Some(j) => j,
                            None => break,
                        };

                        let source_flags = extra_flags.for_source(&source);
                        match compiler.compile(&source, &obj, &source_flags) {
                            Ok(result) if result.success => {
                                let stderr = result.stderr.trim().to_string();
                                if !stderr.is_empty() {
                                    local_warnings.push(stderr);
                                }
                                let count = compiled_count.fetch_add(1, Ordering::Relaxed) + 1;
                                if count % 20 == 0 || count == total {
                                    tracing::info!("[{}/{}] compiled", count, total);
                                    if let Some(log) = build_log {
                                        if let Ok(mut log) = log.lock() {
                                            log.push(format!("Compiled {}/{} files", count, total));
                                        }
                                    }
                                }
                            }
                            Ok(result) => {
                                let mut err = first_error.lock().unwrap();
                                if err.is_none() {
                                    *err = Some(format!(
                                        "compilation failed for {}:\n{}",
                                        source.display(),
                                        result.stderr
                                    ));
                                }
                            }
                            Err(e) => {
                                let mut err = first_error.lock().unwrap();
                                if err.is_none() {
                                    *err = Some(e.to_string());
                                }
                            }
                        }
                    }
                    // Merge local warnings into shared collection
                    if !local_warnings.is_empty() {
                        all_warnings.lock().unwrap().extend(local_warnings);
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }
    });

    if let Some(error) = first_error.into_inner().unwrap() {
        return Err(FbuildError::BuildFailed(error));
    }

    Ok(ParallelCompileResult {
        objects,
        warnings: all_warnings.into_inner().unwrap(),
    })
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
