//! Compile selected framework built-in libraries (WiFi, FS, SPIFFS, Network,
//! etc.) shipped under `framework/libraries/<lib>/src/`.
//!
//! The ESP32 linker cannot reliably garbage-collect every bundled Arduino
//! library: Matter carries global roots that pull its full networking stack into
//! otherwise empty sketches. Callers must therefore pass only the libraries
//! selected by the active-branch LDF resolver (FastLED/fbuild#1449).
//!
//! The work is split in three so the compiles can share the build's one job
//! pool with everything else (FastLED/fbuild#1559):
//! 1. [`prepare_framework_libs`] (sync): cache eviction/hydration, cache hits
//!    and failure-marker skips; decides what still needs compiling.
//! 2. [`FwLibsPlan::compile`] (async): compiles every pending library
//!    concurrently on a shared [`JobGate`]. Touches no perf/build-log state.
//! 3. [`FwLibsPlan::finish`] (sync): applies the outcomes in selected-library
//!    order — cache stores, failure markers, perf checkpoints, build log — and
//!    returns the archives in exactly the order the old serial loop produced.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fbuild_core::Result;
use fbuild_packages::Framework;
use fbuild_packages::library::library_compiler::{JobGate, LibCompileBackend};

use super::super::esp32_compiler::Esp32Compiler;
use super::super::mcu_config::Esp32McuConfig;
use super::framework_library_cache::{FrameworkLibraryCache, hydrate_summary, store_summary};
use super::helpers::{
    framework_failure_marker, framework_signature, record_failed_framework_lib,
    should_skip_failed_framework_lib,
};
use crate::BuildParams;
use crate::compiler::Compiler as _;
use crate::flag_overlay::{LanguageExtraFlags, apply_overlay_flags};
use crate::perf_log::PerfTimer;

/// Everything a framework-library compile needs, fixed before compiling.
pub(super) struct FwLibsContext {
    pub build_dir: PathBuf,
    pub include_dirs: Vec<PathBuf>,
    pub gcc_path: PathBuf,
    pub gxx_path: PathBuf,
    pub ar_path: PathBuf,
    pub c_flags: Vec<String>,
    pub cpp_flags: Vec<String>,
    /// Failure-marker signature (include dirs + flags).
    pub signature: String,
    pub verbose: bool,
    pub compiler_cache: Option<PathBuf>,
    pub compile_cwd: Option<PathBuf>,
    pub backend: Arc<dyn LibCompileBackend>,
    pub cache: FrameworkLibraryCache,
}

/// One selected library that yields an archive (or may), in selection order.
enum FwLibEntry {
    /// The archive is already in the build dir (built earlier or hydrated).
    Cached { archive: PathBuf },
    /// Needs compiling.
    Compile {
        name: String,
        index: usize,
        sources: Vec<PathBuf>,
        marker: PathBuf,
    },
}

/// Framework libraries resolved against the cache; see the module docs.
pub(super) struct FwLibsPlan {
    /// `None` when the framework ships no libraries dir (or `clean_only`).
    ctx: Option<FwLibsContext>,
    entries: Vec<FwLibEntry>,
    /// Time spent in [`prepare_framework_libs`].
    prepare_elapsed: Duration,
}

/// Result of [`FwLibsPlan::compile`]: one outcome per [`FwLibEntry::Compile`]
/// (in entry order), plus the compile's own wall time.
pub(super) struct FwLibsCompiled {
    outcomes: Vec<Result<Option<PathBuf>>>,
    elapsed: Duration,
}

/// Resolve the LDF-selected framework libraries for this build: flags,
/// cache hydrate, cache hits and failure skips. `already_compiled` names the
/// archives a `lib_deps` / project library provides, which are skipped.
#[allow(clippy::too_many_arguments)]
pub(super) fn prepare_framework_libs(
    params: &BuildParams,
    perf: &mut PerfTimer,
    framework: &fbuild_packages::library::Esp32Framework,
    toolchain: &fbuild_packages::toolchain::Esp32Toolchain,
    mcu_config: &Esp32McuConfig,
    board: &fbuild_config::BoardConfig,
    build_unflags: &[String],
    eh_frame_policy: crate::eh_frame_policy::EhFramePolicy,
    include_dirs: &[PathBuf],
    user_overlay: &LanguageExtraFlags,
    build_dir: &Path,
    compiler_cache: Option<&Path>,
    selected_libraries: &[fbuild_packages::library::FrameworkLibrary],
    already_compiled: &HashSet<String>,
    build_log: &mut fbuild_core::BuildLog,
) -> Result<FwLibsPlan> {
    use fbuild_packages::Toolchain;

    let started = Instant::now();
    perf.checkpoint("fw-libs-start");
    let builtin_libs_dir = framework.get_libraries_dir();
    if !builtin_libs_dir.is_dir() {
        return Ok(FwLibsPlan::empty(started.elapsed()));
    }

    let fw_libs_build_dir = build_dir.join("fw_libs");
    std::fs::create_dir_all(&fw_libs_build_dir)?;

    // Compile framework libs workspace-relative so their zccache keys are
    // stable across project directories and hit the warm cache instead of
    // recompiling ~150s on every fresh project (FastLED/fbuild#952). The
    // object dir lives under `<project>/.fbuild/...`, so this resolves to
    // the project workspace root (canonicalized).
    let fw_compile_cwd =
        fbuild_core::path::compile_cwd_from_output(&fw_libs_build_dir.join("obj").join("_probe.o"));

    // Get compiler flags for framework library compilation
    let mut fw_defines = board.get_defines();
    fw_defines.extend(mcu_config.defines_map());

    let fw_compiler = Esp32Compiler::with_temp_dir(
        toolchain.get_gcc_path(),
        toolchain.get_gxx_path(),
        mcu_config.clone(),
        &board.f_cpu,
        fw_defines,
        include_dirs.to_vec(),
        params.profile,
        params.verbose,
        build_dir.join("tmp"),
    )
    .with_build_unflags(build_unflags.to_vec())
    .with_eh_frame_policy(eh_frame_policy);
    let fw_c_flags = apply_overlay_flags(&fw_compiler.c_flags(), user_overlay, "dummy.c");
    let fw_cpp_flags = apply_overlay_flags(&fw_compiler.cpp_flags(), user_overlay, "dummy.cpp");
    let fw_signature = framework_signature(include_dirs, &fw_c_flags, &fw_cpp_flags);
    let cache = FrameworkLibraryCache::new(
        &params.project_dir,
        params.profile,
        &fw_signature,
        &builtin_libs_dir,
    );
    // Use gcc-ar for LTO archives so the linker-plugin index is written.
    let ar_path = crate::pipeline::pick_archiver(
        &toolchain.get_ar_path(),
        &toolchain.get_gcc_ar_path(),
        &fw_c_flags,
        &fw_cpp_flags,
    )
    .to_path_buf();
    let ctx = FwLibsContext {
        build_dir: fw_libs_build_dir,
        include_dirs: include_dirs.to_vec(),
        gcc_path: toolchain.get_gcc_path(),
        gxx_path: toolchain.get_gxx_path(),
        ar_path,
        c_flags: fw_c_flags,
        cpp_flags: fw_cpp_flags,
        signature: fw_signature,
        verbose: params.verbose,
        compiler_cache: compiler_cache.map(Path::to_path_buf),
        compile_cwd: fw_compile_cwd,
        // FastLED/fbuild#986: route framework-lib TU compiles through the
        // in-process embedded zccache service (the same path sketch/core use)
        // so they are cached and hit cross-project via #985's
        // project-independent keys.
        backend: Arc::new(crate::compile_backend::EmbeddedLibBackend),
        cache,
    };
    let mut plan = FwLibsPlan::new(
        ctx,
        selected_libraries,
        already_compiled,
        params.clean_all,
        params.clean_only,
        perf,
        build_log,
    )?;
    plan.prepare_elapsed = started.elapsed();
    Ok(plan)
}

impl FwLibsPlan {
    fn empty(prepare_elapsed: Duration) -> Self {
        Self {
            ctx: None,
            entries: Vec::new(),
            prepare_elapsed,
        }
    }

    /// Evict (`clean_all`) / hydrate the cache, then classify each selected
    /// library in order: skipped, already archived, or to compile.
    pub(super) fn new(
        ctx: FwLibsContext,
        selected_libraries: &[fbuild_packages::library::FrameworkLibrary],
        already_compiled: &HashSet<String>,
        clean_all: bool,
        clean_only: bool,
        perf: &mut PerfTimer,
        build_log: &mut fbuild_core::BuildLog,
    ) -> Result<Self> {
        if clean_all {
            match ctx.cache.remove() {
                Ok(()) => tracing::info!("removed ESP32 framework library cache"),
                Err(error) => {
                    tracing::warn!("failed to remove ESP32 framework library cache: {}", error)
                }
            }
        }
        if clean_only {
            return Ok(Self::empty(Duration::ZERO));
        }
        let hydrate_outcome = ctx.cache.hydrate(&ctx.build_dir);
        build_log.push(hydrate_summary(&hydrate_outcome));
        match &hydrate_outcome {
            Ok(copied) if *copied > 0 => tracing::info!(
                "hydrated {} cached ESP32 framework library archives",
                copied
            ),
            Ok(_) => {}
            Err(error) => {
                tracing::warn!("failed to hydrate ESP32 framework library cache: {}", error)
            }
        }

        let mut entries = Vec::new();
        let mut seen = 0;
        for library in selected_libraries {
            let lib_name = library.name.to_lowercase();
            if already_compiled.contains(&lib_name) {
                continue;
            }
            seen += 1;

            // Check if archive already exists
            let archive = ctx.build_dir.join(format!("lib{}.a", lib_name));
            if archive.exists() {
                if perf.is_active() {
                    perf.checkpoint(format!("fw-lib-cache-hit name={} index={}", lib_name, seen));
                }
                entries.push(FwLibEntry::Cached { archive });
                continue;
            }

            let sources = &library.source_files;
            if sources.is_empty() {
                continue;
            }
            let marker = framework_failure_marker(&ctx.build_dir, &lib_name);
            if should_skip_failed_framework_lib(&marker, &ctx.signature, sources)? {
                if perf.is_active() {
                    perf.checkpoint(format!(
                        "fw-lib-skip-failed name={} index={} sources={}",
                        lib_name,
                        seen,
                        sources.len()
                    ));
                }
                tracing::debug!(
                    "skipping previously failed framework library '{}'",
                    lib_name
                );
                continue;
            }
            if ctx.cache.has_failed(&lib_name) {
                if perf.is_active() {
                    perf.checkpoint(format!(
                        "fw-lib-cache-skip-failed name={} index={}",
                        lib_name, seen
                    ));
                }
                continue;
            }
            if perf.is_active() {
                perf.checkpoint(format!(
                    "fw-lib-compile-queued name={} index={} sources={}",
                    lib_name,
                    seen,
                    sources.len()
                ));
            }
            entries.push(FwLibEntry::Compile {
                name: lib_name,
                index: seen,
                sources: sources.clone(),
                marker,
            });
        }
        Ok(Self {
            ctx: Some(ctx),
            entries,
            prepare_elapsed: Duration::ZERO,
        })
    }

    /// Compile every pending library concurrently, each translation unit
    /// drawing a permit from `gate`. A failing library does not stop the
    /// others: framework-library failures are tolerated (see [`Self::finish`]).
    pub(super) async fn compile(&self, gate: &JobGate) -> FwLibsCompiled {
        let started = Instant::now();
        let Some(ctx) = &self.ctx else {
            return FwLibsCompiled {
                outcomes: Vec::new(),
                elapsed: Duration::ZERO,
            };
        };
        let compiles = self.entries.iter().filter_map(|entry| match entry {
            FwLibEntry::Cached { .. } => None,
            FwLibEntry::Compile { name, sources, .. } => Some(async move {
                fbuild_packages::library::library_compiler::compile_library_gated(
                    name,
                    sources,
                    &ctx.include_dirs,
                    &ctx.gcc_path,
                    &ctx.gxx_path,
                    &ctx.ar_path,
                    &ctx.c_flags,
                    &ctx.cpp_flags,
                    &ctx.build_dir,
                    ctx.verbose,
                    gate,
                    ctx.compiler_cache.as_deref(),
                    ctx.compile_cwd.clone(),
                    Some(ctx.backend.clone()),
                    None,
                )
                .await
            }),
        });
        // join_all runs every compile to completion even if one fails.
        let outcomes = futures::future::join_all(compiles).await;
        FwLibsCompiled {
            outcomes,
            elapsed: started.elapsed(),
        }
    }

    /// Apply compile outcomes in selected-library order and return the
    /// archives to link, cache hits and fresh compiles interleaved exactly as
    /// selected. Failures are recorded (failure marker + cache) and skipped:
    /// some framework libraries do not build for every chip, and the linker
    /// reports any symbol that was actually needed.
    pub(super) fn finish(
        self,
        compiled: FwLibsCompiled,
        perf: &mut PerfTimer,
        build_log: &mut fbuild_core::BuildLog,
    ) -> Vec<PathBuf> {
        let finish_started = Instant::now();
        let mut archives = Vec::new();
        let Some(ctx) = self.ctx else {
            perf.record("fw-libs", self.prepare_elapsed + compiled.elapsed);
            perf.checkpoint("fw-libs-finish");
            return archives;
        };
        let mut outcomes = compiled.outcomes.into_iter();
        let mut stored = 0;
        for entry in self.entries {
            let (name, index, marker) = match entry {
                FwLibEntry::Cached { archive } => {
                    archives.push(archive);
                    continue;
                }
                FwLibEntry::Compile {
                    name,
                    index,
                    marker,
                    ..
                } => (name, index, marker),
            };
            let outcome = outcomes.next().unwrap_or_else(|| {
                Err(fbuild_core::FbuildError::BuildFailed(
                    "framework library was not compiled".into(),
                ))
            });
            match outcome {
                Ok(Some(archive)) => {
                    let _ = std::fs::remove_file(&marker);
                    match ctx.cache.store_archive(&archive) {
                        Ok(()) => stored += 1,
                        Err(error) => {
                            tracing::warn!("failed to cache framework library {}: {}", name, error)
                        }
                    }
                    archives.push(archive);
                    if perf.is_active() {
                        perf.checkpoint(format!(
                            "fw-lib-compile-finish name={} index={} count={}",
                            name,
                            index,
                            archives.len()
                        ));
                    }
                }
                Ok(None) => {
                    if perf.is_active() {
                        perf.checkpoint(format!(
                            "fw-lib-header-only name={} index={}",
                            name, index
                        ));
                    }
                }
                Err(e) => {
                    if perf.is_active() {
                        perf.checkpoint(format!(
                            "fw-lib-compile-error name={} index={}",
                            name, index
                        ));
                    }
                    tracing::debug!("framework library {} failed to compile: {}", name, e);
                    record_failed_framework_lib(&marker, &ctx.signature, &e.to_string());
                    if let Err(error) = ctx.cache.record_failure(&name) {
                        tracing::warn!(
                            "failed to cache framework library failure {}: {}",
                            name,
                            error
                        );
                    }
                }
            }
        }

        if !archives.is_empty() {
            tracing::info!("compiled {} framework built-in libraries", archives.len());
        }
        build_log.push(store_summary(stored));
        perf.record(
            "fw-libs",
            self.prepare_elapsed + compiled.elapsed + finish_started.elapsed(),
        );
        perf.checkpoint("fw-libs-finish");
        archives
    }
}

#[cfg(test)]
#[path = "framework_libs_tests.rs"]
pub(super) mod tests;
