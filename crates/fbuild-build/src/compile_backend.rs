//! Backend selector for zccache compile dispatch.
//!
//! Phase 1 of FastLED/fbuild#789 (this module, #790): the enum is
//! constructed at daemon startup from the `FBUILD_ZCCACHE_EMBEDDED`
//! env var and held on `DaemonContext`. There is no per-compile
//! routing yet — the existing wrapper-spawn path in
//! [`crate::zccache`] runs regardless. Phase 2 (#791) wires routing
//! through `CompileBackend::Embedded`; Phase 4 (#793) flips embedded
//! to default and retires the wrapper variant.
//!
//! The `Embedded` variant only exists when the `embedded` Cargo
//! feature is enabled. Without that feature the runtime opt-in
//! gracefully degrades to `Wrapped` with a `tracing::warn!`.

#[cfg(feature = "embedded")]
use std::sync::Arc;

#[cfg(feature = "embedded")]
use crate::zccache_embedded::{EmbeddedServiceError, FbuildZccacheService};

/// The active zccache compile backend for the running daemon.
///
/// Resolved once at daemon startup from [`CompileBackend::from_env`].
/// Cheap to clone — the embedded variant wraps an `Arc<ZccacheService>`
/// internally, so the clone is reference-counted.
#[derive(Clone, Default)]
pub enum CompileBackend {
    /// Default: every compile spawns `zccache wrap <compiler> <args>`
    /// against the managed wrapper binary (see [`crate::zccache`] +
    /// [`crate::managed_zccache`]).
    #[default]
    Wrapped,

    /// In-process: compiles dispatch through `ZccacheService::compile`
    /// inside the daemon's tokio runtime. Constructed by
    /// [`CompileBackend::from_env`] when
    /// `FBUILD_ZCCACHE_EMBEDDED=1` is set AND the binary was built
    /// with the `embedded` Cargo feature.
    #[cfg(feature = "embedded")]
    Embedded(Arc<FbuildZccacheService>),
}

impl std::fmt::Debug for CompileBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

impl CompileBackend {
    /// Stable name for logs / diagnostics. `"wrapped"` or `"embedded"`.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Wrapped => "wrapped",
            #[cfg(feature = "embedded")]
            Self::Embedded(_) => "embedded",
        }
    }

    /// Resolve the backend at daemon startup.
    ///
    /// Reads `FBUILD_ZCCACHE_EMBEDDED` (truthy = `"1"`):
    /// - `=1` AND `embedded` feature on → starts `ZccacheService` and
    ///   returns `Embedded`. If the service fails to start, logs a
    ///   warning and falls back to `Wrapped`.
    /// - `=1` AND `embedded` feature off → logs a warning that the
    ///   binary was built without embedded support, returns
    ///   `Wrapped`.
    /// - unset / any other value → returns `Wrapped`.
    ///
    /// Must be called from inside the daemon's tokio runtime — the
    /// embedded service's `start` is `async` and spawns background
    /// tasks via `tokio::spawn`.
    pub async fn from_env() -> Self {
        let want_embedded = std::env::var("FBUILD_ZCCACHE_EMBEDDED")
            .map(|v| v == "1")
            .unwrap_or(false);

        if !want_embedded {
            tracing::info!("zccache backend: wrapped (default)");
            return Self::Wrapped;
        }

        #[cfg(feature = "embedded")]
        {
            match FbuildZccacheService::start().await {
                Ok(svc) => {
                    tracing::info!(
                        "zccache backend: embedded \
                         (FBUILD_ZCCACHE_EMBEDDED=1, cache_root={})",
                        svc.cache_root().display()
                    );
                    return Self::Embedded(Arc::new(svc));
                }
                Err(EmbeddedServiceError::Start(err)) => {
                    tracing::warn!(
                        "FBUILD_ZCCACHE_EMBEDDED=1 but embedded service \
                         failed to start: {err}; falling back to wrapped"
                    );
                }
                Err(other) => {
                    tracing::warn!(
                        "FBUILD_ZCCACHE_EMBEDDED=1 but embedded service \
                         start raised an unexpected error: {other}; \
                         falling back to wrapped"
                    );
                }
            }
        }
        #[cfg(not(feature = "embedded"))]
        {
            tracing::warn!(
                "FBUILD_ZCCACHE_EMBEDDED=1 but this binary was built \
                 without --features fbuild-build/embedded; falling back \
                 to wrapped. Rebuild with the feature to enable \
                 in-process zccache."
            );
        }

        Self::Wrapped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `name()` returns a stable, lower-case string that diagnostics +
    /// log lines can rely on. The string is part of the
    /// daemon-startup log contract documented in #790's acceptance
    /// criteria ("daemon logs which backend is active").
    #[test]
    fn name_is_wrapped_by_default() {
        assert_eq!(CompileBackend::Wrapped.name(), "wrapped");
        assert_eq!(CompileBackend::default().name(), "wrapped");
    }

    /// `from_env` with the env var unset / not "1" returns `Wrapped`
    /// regardless of the `embedded` feature flag. The opt-in is
    /// exact-match `"1"`; anything else is no-op.
    #[tokio::test(flavor = "current_thread")]
    async fn from_env_returns_wrapped_when_var_unset() {
        let prior = std::env::var_os("FBUILD_ZCCACHE_EMBEDDED");
        unsafe { std::env::remove_var("FBUILD_ZCCACHE_EMBEDDED") };
        let backend = CompileBackend::from_env().await;
        assert_eq!(backend.name(), "wrapped");
        if let Some(v) = prior {
            unsafe { std::env::set_var("FBUILD_ZCCACHE_EMBEDDED", v) };
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn from_env_returns_wrapped_when_var_is_truthy_string_not_one() {
        let prior = std::env::var_os("FBUILD_ZCCACHE_EMBEDDED");
        unsafe { std::env::set_var("FBUILD_ZCCACHE_EMBEDDED", "true") };
        let backend = CompileBackend::from_env().await;
        assert_eq!(backend.name(), "wrapped");
        match prior {
            Some(v) => unsafe { std::env::set_var("FBUILD_ZCCACHE_EMBEDDED", v) },
            None => unsafe { std::env::remove_var("FBUILD_ZCCACHE_EMBEDDED") },
        }
    }
}
