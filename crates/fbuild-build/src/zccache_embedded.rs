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

use std::path::PathBuf;
use std::sync::Arc;

use zccache::audit::AuditMode;
use zccache::embedded::{
    AuditConfig, HostIdentity, RuntimeHooks, ServiceLimits, ShutdownMode, ZccacheConfig,
    ZccacheService,
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
    #[error("io while preparing zccache cache root: {0}")]
    Io(#[from] std::io::Error),
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

    /// Drain pending writes. Useful at end-of-session boundaries.
    pub async fn flush(&self) -> Result<(), EmbeddedServiceError> {
        self.inner
            .flush()
            .await
            .map(|_| ())
            .map_err(|e| EmbeddedServiceError::Flush(e.to_string()))
    }

    /// Graceful shutdown. Called from the daemon's normal exit path.
    ///
    /// If other clones of the underlying `Arc<ZccacheService>` exist
    /// (which Phase 1 does not exercise but Phase 2 will once it
    /// holds handles in per-compile call sites), fall back to a
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
