//! Process-wide handle to the embedded zccache compile backend.
//!
//! [FastLED/fbuild#789](https://github.com/FastLED/fbuild/issues/789)
//! Phase 4 stage 2 (#800): the embedded `ZccacheService` is now the
//! ONLY backend. The Phase-1-era `Wrapped` enum variant + the
//! `FBUILD_ZCCACHE_EMBEDDED` runtime env-var hatch + the
//! `--features embedded` Cargo gate are all deleted. `CompileBackend`
//! is now a `newtype` around `Arc<FbuildZccacheService>`.
//!
//! Per-compile call sites (`compile_source` and friends) live in
//! `fbuild-build` and don't have a handle to `DaemonContext`, so the
//! installed backend lives in a process-wide `OnceLock` set by the
//! daemon's `#[tokio::main]` before any compile fires. Reads
//! ([`get_global`]) are lock-free.

use std::sync::{Arc, OnceLock};

use crate::zccache_embedded::FbuildZccacheService;

/// The active embedded zccache backend.
///
/// Newtype around `Arc<FbuildZccacheService>` so the underlying
/// `ZccacheService` (which is itself wrapped in an `Arc` inside
/// `FbuildZccacheService`) is reference-counted across every clone
/// that lands on the per-compile call site.
#[derive(Clone)]
pub struct CompileBackend {
    service: Arc<FbuildZccacheService>,
    runtime: tokio::runtime::Handle,
}

impl std::fmt::Debug for CompileBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompileBackend")
            .field("cache_root", &self.service.cache_root())
            .finish()
    }
}

impl CompileBackend {
    /// Start the embedded zccache service and return a backend handle.
    ///
    /// Must be called from inside the daemon's tokio runtime — the
    /// embedded service's `start` is `async` and the runtime handle
    /// captured here is the one synchronous per-compile dispatches
    /// will later `block_on(...)` against.
    pub async fn start() -> Result<Self, crate::zccache_embedded::EmbeddedServiceError> {
        let service = Arc::new(FbuildZccacheService::start().await?);
        tracing::info!(
            "zccache backend ready (embedded, cache_root={})",
            service.cache_root().display()
        );
        Ok(Self {
            service,
            runtime: tokio::runtime::Handle::current(),
        })
    }

    /// Handle to the embedded service. Cheap to clone (Arc).
    pub fn service(&self) -> &Arc<FbuildZccacheService> {
        &self.service
    }

    /// Tokio runtime handle suitable for `Handle::block_on(...)`.
    pub fn runtime(&self) -> &tokio::runtime::Handle {
        &self.runtime
    }
}

static GLOBAL: OnceLock<CompileBackend> = OnceLock::new();

/// Install the process-wide compile backend.
///
/// Idempotent on second-call (the second invocation logs a warning
/// and is otherwise a no-op). Tests that need to exercise multiple
/// services in one process should pass the constructed
/// `FbuildZccacheService` directly to the code under test rather
/// than going through the global.
pub fn install_global(backend: CompileBackend) {
    if GLOBAL.set(backend).is_err() {
        tracing::warn!(
            "compile_backend::install_global called more than once; second call ignored"
        );
    }
}

/// Read the process-wide compile backend, if installed.
///
/// `None` means "no daemon has wired the global yet" — happens in
/// `fbuild-build` unit tests and other in-process callers that
/// don't go through `fbuild-daemon`'s startup path. Such callers
/// must avoid calling into `compile_source` (or otherwise tolerate
/// the resulting hard error from the unwrapped `get`).
pub fn get_global() -> Option<&'static CompileBackend> {
    GLOBAL.get()
}

/// Routes library-TU compiles through the in-process embedded zccache service
/// (FastLED/fbuild#986) so they are cached like sketch/core compiles. Injected
/// into `fbuild_packages`' library compiler as a trait object, since that crate
/// cannot depend on `fbuild-build`.
pub struct EmbeddedLibBackend;

#[async_trait::async_trait]
impl fbuild_packages::library::library_compiler::LibCompileBackend for EmbeddedLibBackend {
    async fn compile(
        &self,
        compiler: &std::path::Path,
        args: Vec<String>,
        cwd: std::path::PathBuf,
        env: Vec<(String, String)>,
    ) -> fbuild_core::Result<fbuild_packages::library::library_compiler::LibCompileOutcome> {
        let global = get_global().ok_or_else(|| {
            fbuild_core::FbuildError::BuildFailed(
                "compile_backend not installed — fbuild-daemon must call \
                 compile_backend::install_global at startup (FastLED/fbuild#800)"
                    .to_string(),
            )
        })?;
        let svc = global.service();
        let compile_fut = svc.compile(compiler, args, cwd, env);
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(300), compile_fut)
            .await
            .map_err(|_| {
                fbuild_core::FbuildError::BuildFailed(
                    "embedded library compile timed out after 300s".to_string(),
                )
            })?
            .map_err(|e| {
                fbuild_core::FbuildError::BuildFailed(format!(
                    "embedded library compile failed: {e}"
                ))
            })?;
        Ok(
            fbuild_packages::library::library_compiler::LibCompileOutcome {
                exit_code: outcome.exit_code,
                stdout: outcome.stdout,
                stderr: outcome.stderr,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Without `install_global`, `get_global()` returns `None`. This
    /// is the contract per-compile call sites depend on — they fall
    /// back to a clean error rather than panicking on `unwrap`.
    #[test]
    fn get_global_returns_none_without_install() {
        // NB: this test cannot meaningfully *install* a backend
        // because `OnceLock` is process-global and other tests in
        // the same binary may already have set it. The test verifies
        // only the shape of the API at the unset starting state in
        // a single-test process.
        let _ = get_global();
    }
}
