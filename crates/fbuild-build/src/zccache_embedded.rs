//! Embedded zccache service wrapper.
//!
//! Phase 1 of FastLED/fbuild#789 (this module, #790): hosts the
//! upstream `zccache::embedded::ZccacheService` inside fbuild-daemon's
//! tokio runtime so a future phase can route per-compile dispatch
//! through it without spawning a `zccache wrap …` child process per
//! TU. Phase 1 only constructs the service — Phase 2 (#791) wires
//! the dispatch.
//!
//! ## Design constraint: daemon-only
//!
//! The embedded `ZccacheService` lives **inside the long-lived
//! fbuild-daemon process and nowhere else**. Transient processes
//! (the CLI, build orchestrators called outside the daemon) keep
//! talking to the wrapper binary because paying
//! `ZccacheService::start` for a one-shot invocation would erase
//! every saving the embedded model offers.
//!
//! ## Tokio runtime sharing
//!
//! `ZccacheConfig::runtime.handle` is left `None` for Phase 1.
//! `ZccacheService::start` is `async`, so the persistent background
//! tasks it spawns via `tokio::spawn` land on the **ambient**
//! runtime — which means the daemon's tokio runtime. Single-runtime
//! attach for tokio-console therefore works without explicit
//! handle plumbing. The explicit-handle path
//! ([zccache#922](https://github.com/zackees/zccache/issues/922))
//! is a Phase 2+ refinement.
//!
//! ## Identity defaults
//!
//! Constructed via [`HostIdentity::default_for_product("fbuild")`],
//! which hashes the current exe path so two fbuild installs at
//! different paths get distinct cache identities while an
//! upgrade-in-place keeps cache continuity. See zccache#925 for the
//! identity contract.
//!
//! ## Cache root
//!
//! `<fbuild_root>/zccache/` (i.e. `~/.fbuild/<mode>/zccache/`).
//! Created if missing during [`FbuildZccacheService::start`].

use std::path::{Path, PathBuf};
use std::sync::Arc;

use zccache::audit::{AuditContext, AuditId, AuditMode};
use zccache::embedded::{
    AuditConfig, CompileRequest as ZccacheCompileRequest, HostIdentity, RuntimeHooks, ServiceLimits,
    ShutdownMode, ZccacheConfig, ZccacheService,
};
use zccache::fingerprint::{
    decision::CacheDecision, scan::walk_files, TwoLayerCache,
};

/// fbuild-side handle around a started [`ZccacheService`].
///
/// Cheap to clone — the underlying `ZccacheService` is wrapped in an
/// `Arc`, so cloning is reference-counting only.
pub struct FbuildZccacheService {
    inner: Arc<ZccacheService>,
    identity: HostIdentity,
    cache_root: PathBuf,
}

/// Errors raised while starting / flushing / shutting the embedded
/// service. Wraps upstream errors as plain strings so callers outside
/// this module don't need to import any `zccache::*` types.
#[derive(Debug, thiserror::Error)]
pub enum EmbeddedServiceError {
    #[error("zccache embedded start failed: {0}")]
    Start(String),
    #[error("zccache embedded flush failed: {0}")]
    Flush(String),
    #[error("zccache embedded shutdown failed: {0}")]
    Shutdown(String),
    /// Per-compile dispatch (Phase 2 / #791) — the underlying
    /// `ZccacheService::compile` failed. Surfaced so the
    /// `compile_source` call site can log a warning and fall back to
    /// the wrapper path.
    #[error("zccache embedded compile failed: {0}")]
    Compile(String),
    #[error("io while preparing zccache cache root: {0}")]
    Io(#[from] std::io::Error),
}

/// Per-compile outcome surfaced to fbuild's wrapper-mode call sites
/// (Phase 2 / #791). Mirrors the shape of `CompileResult` field-by-
/// field minus the path/object metadata that's not zccache-owned.
#[derive(Debug, Clone)]
pub struct EmbeddedCompileOutcome {
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub cached: bool,
}

impl FbuildZccacheService {
    /// Start the embedded service on the caller's tokio runtime,
    /// rooted at `~/.fbuild/<mode>/zccache/`.
    ///
    /// Idempotent only at the file-system level — `create_dir_all`
    /// on the cache root is safe under concurrent callers. Two
    /// concurrent `start()` calls would each spawn an
    /// independent `ZccacheService` with the same identity; the
    /// daemon's startup path is single-threaded so this is not
    /// exercised today.
    pub async fn start() -> Result<Self, EmbeddedServiceError> {
        Self::start_in(fbuild_paths::get_fbuild_root().join("zccache")).await
    }

    /// Start with an explicit cache root.
    ///
    /// Production callers should use [`Self::start`], which derives
    /// the root from `fbuild_paths`. This entry point exists so the
    /// smoke test (`tests/zccache_embedded_smoke.rs`) can point at a
    /// per-test tempdir and not contaminate the user's real
    /// `~/.fbuild/<mode>/zccache/`. Phase 2 (#791) may also use this
    /// to host multiple service instances per integration test.
    pub async fn start_in(cache_root: PathBuf) -> Result<Self, EmbeddedServiceError> {
        std::fs::create_dir_all(&cache_root)?;
        let identity = HostIdentity::default_for_product("fbuild");

        // zccache#926: `AuditConfig::default()` ships `mode = Normal`
        // + `output_root = None`, which the strict-validation pass
        // rejects ("audit sink requires output_root when mode > Off").
        // fbuild does not consume zccache audit events today, so set
        // mode = Off explicitly. The daemon's own tracing layer
        // captures everything fbuild cares about.
        let audit = AuditConfig {
            mode: AuditMode::Off,
            ..AuditConfig::default()
        };
        let cfg = ZccacheConfig {
            host: identity.clone(),
            cache_root: cache_root.clone().into(),
            audit,
            limits: ServiceLimits::default(),
            runtime: RuntimeHooks {
                service_name: Some("fbuild-daemon".into()),
                // Leave None for Phase 1; tasks land on the ambient
                // runtime which is the daemon's. zccache#922 follow-up
                // adds explicit handle plumbing.
                handle: None,
            },
            // Leave None — the daemon's existing shutdown signal
            // races completion in fbuild's own code; the embedded
            // service doesn't need its own cancellation token for
            // Phase 1.
            cancellation: None,
        };

        let svc = ZccacheService::start(cfg)
            .await
            .map_err(|e| EmbeddedServiceError::Start(e.to_string()))?;
        Ok(Self {
            inner: Arc::new(svc),
            identity,
            cache_root,
        })
    }

    /// Resolved on-disk cache root for this service.
    pub fn cache_root(&self) -> &std::path::Path {
        &self.cache_root
    }

    /// Stable host identity used to derive cache keys.
    pub fn identity(&self) -> &HostIdentity {
        &self.identity
    }

    /// Internal handle to the underlying `ZccacheService` for the
    /// sync-from-blocking call path. Phase 2 (#791) only — the field
    /// itself is private.
    pub(crate) fn inner(&self) -> &Arc<ZccacheService> {
        &self.inner
    }

    /// Synchronous per-compile dispatch (Phase 2 / FastLED/fbuild#791).
    ///
    /// Builds a [`ZccacheCompileRequest`] from the wrapper-mode
    /// inputs, blocks on the underlying async
    /// [`ZccacheService::compile`] via the caller-supplied
    /// `tokio::runtime::Handle`, and returns an
    /// [`EmbeddedCompileOutcome`] shaped to drop into
    /// [`crate::compiler::CompileResult`]. Designed to slot into
    /// `compile_source` exactly where the wrapper-mode
    /// `run_command(["zccache", "wrap", compiler, …])` runs today.
    ///
    /// `runtime` must be a multi-thread tokio runtime handle — the
    /// daemon's `#[tokio::main]` default. Calling from inside a
    /// current-thread runtime will panic per tokio's `Handle::block_on`
    /// contract.
    ///
    /// `env` is forwarded as a `Vec<(String, String)>` — zccache's
    /// embedded API takes the same shape. fbuild does NOT inherit
    /// caller env here; the call site must pass exactly the env vars
    /// the compile needs (typically the empty vec — gcc reads no env
    /// fbuild cares about for caching).
    pub fn compile_blocking(
        &self,
        runtime: &tokio::runtime::Handle,
        compiler: &Path,
        args: Vec<String>,
        cwd: PathBuf,
        env: Vec<(String, String)>,
    ) -> Result<EmbeddedCompileOutcome, EmbeddedServiceError> {
        let req = ZccacheCompileRequest {
            audit: default_audit_context(),
            compiler: compiler.to_path_buf().into(),
            args,
            cwd: cwd.into(),
            env,
            stdin: Vec::new(),
        };
        let inner = self.inner.clone();
        let resp = runtime
            .block_on(async move { inner.compile(req).await })
            .map_err(|e| EmbeddedServiceError::Compile(e.to_string()))?;
        Ok(EmbeddedCompileOutcome {
            exit_code: resp.exit_code,
            stdout: resp.stdout,
            stderr: resp.stderr,
            cached: resp.cached,
        })
    }

    /// Drain pending writes. Useful at end-of-session boundaries.
    pub async fn flush(&self) -> Result<(), EmbeddedServiceError> {
        self.inner
            .flush()
            .await
            .map(|_| ())
            .map_err(|e| EmbeddedServiceError::Flush(e.to_string()))
    }

    /// Suppress dead-code warnings under the embedded-feature build
    /// for the (intentionally currently-unused) `inner` accessor. The
    /// helper exists for Phase 3+ callers that need direct
    /// `ZccacheService` access (e.g. the fingerprint API in #792).
    #[doc(hidden)]
    #[allow(dead_code)]
    fn _keep_inner_alive(&self) -> &Arc<ZccacheService> {
        self.inner()
    }

    /// Graceful shutdown. Called from the daemon's normal exit path.
    ///
    /// If other clones of the underlying `Arc<ZccacheService>` exist
    /// (Phase 2 holds one per `GlobalCompileBackend`), fall back to a
    /// `flush()` and let `Drop` reap the rest.
    pub async fn shutdown(self, mode: ShutdownMode) -> Result<(), EmbeddedServiceError> {
        match Arc::try_unwrap(self.inner) {
            Ok(svc) => svc
                .shutdown(mode)
                .await
                .map(|_| ())
                .map_err(|e| EmbeddedServiceError::Shutdown(e.to_string())),
            Err(arc) => {
                let _ = arc.flush().await;
                Ok(())
            }
        }
    }
}

/// Embedded fingerprint check (Phase 3 of FastLED/fbuild#789 / #792).
///
/// Drives the upstream `TwoLayerCache` directly instead of shelling
/// out to `zccache fp check`. Returns the wrapper-mode-equivalent
/// `(Changed | Unchanged)` verdict so the caller in
/// [`crate::zccache::check_fingerprint`] can dispatch to either path
/// behind a single API.
///
/// Synchronous because `TwoLayerCache::check` is synchronous —
/// fingerprint walking is rayon-parallel internally but no tokio
/// runtime is involved.
pub fn check_fingerprint_embedded(
    cache_file: &Path,
    root: &Path,
    extensions: &[String],
    excludes: &[String],
) -> Result<EmbeddedFingerprintCheck, EmbeddedServiceError> {
    if let Some(parent) = cache_file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let ext_refs: Vec<&str> = extensions.iter().map(String::as_str).collect();
    let exc_refs: Vec<&str> = excludes.iter().map(String::as_str).collect();
    let files = walk_files(root, &ext_refs, &exc_refs)
        .map_err(|e| EmbeddedServiceError::Compile(format!("fp walk_files: {e}")))?;
    let cache = TwoLayerCache::new(cache_file.to_path_buf());
    let decision = cache
        .check(&files)
        .map_err(|e| EmbeddedServiceError::Compile(format!("fp check: {e}")))?;
    Ok(match decision {
        CacheDecision::Skip => EmbeddedFingerprintCheck::Unchanged,
        CacheDecision::Run(_) => EmbeddedFingerprintCheck::Changed,
    })
}

/// Promote the pending fingerprint snapshot written by the last
/// [`check_fingerprint_embedded`] call to the success state.
pub fn mark_fingerprint_success_embedded(cache_file: &Path) -> Result<(), EmbeddedServiceError> {
    if let Some(parent) = cache_file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let cache = TwoLayerCache::new(cache_file.to_path_buf());
    cache
        .mark_success()
        .map_err(|e| EmbeddedServiceError::Compile(format!("fp mark_success: {e}")))
}

/// Conservative fingerprint verdict (Phase 3 / #792). Matches the
/// shape of the wrapper-mode [`crate::zccache::FingerprintCheck`]
/// enum so the caller can normalize without an intermediate type
/// in non-embedded builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddedFingerprintCheck {
    Changed,
    Unchanged,
}

/// Per-call `AuditContext` for fbuild-issued compiles.
///
/// fbuild does not consume zccache audit events (the host service is
/// configured with `AuditMode::Off`), so the IDs here are placeholders
/// — `ZccacheService::compile` uses them only as opaque correlation
/// values. A 32-hex-char synthetic id is plenty unique for the
/// in-process case and avoids a new uuid-on-call dep.
fn default_audit_context() -> AuditContext {
    let id = synthetic_audit_id();
    let run_id = AuditId::new(id.clone()).expect("non-empty audit id");
    let trace_id = AuditId::new(id).expect("non-empty audit id");
    AuditContext::new(run_id, trace_id)
}

fn synthetic_audit_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    // blake3-hash (pid, nanos) → first 16 bytes → hex. Avoids the uuid
    // crate purely for this module while still being adequately unique
    // for per-call correlation.
    let mut hasher = blake3::Hasher::new();
    hasher.update(&pid.to_le_bytes());
    hasher.update(&nanos.to_le_bytes());
    let bytes = hasher.finalize();
    let mut hex = String::with_capacity(32);
    for byte in &bytes.as_bytes()[..16] {
        use std::fmt::Write;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}
