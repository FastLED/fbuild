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

use std::sync::OnceLock;

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
    /// **Phase 4 default flip (FastLED/fbuild#789 / #793):** the
    /// embedded backend is now the default. The env var has flipped
    /// from an opt-in (`=1` to enable) to an opt-out (`=0` to
    /// disable). Reads `FBUILD_ZCCACHE_EMBEDDED`:
    /// - unset / `=1` / any non-`"0"` value AND `embedded` feature on
    ///   → starts `ZccacheService` and returns `Embedded`. If the
    ///   service fails to start, logs a warning and falls back to
    ///   `Wrapped`.
    /// - `=0` → returns `Wrapped` (explicit opt-out, e.g. for
    ///   debugging the wrapper-mode code path).
    /// - unset AND `embedded` feature off → returns `Wrapped` (the
    ///   `embedded` Cargo feature wasn't compiled in).
    ///
    /// Must be called from inside the daemon's tokio runtime — the
    /// embedded service's `start` is `async` and spawns background
    /// tasks via `tokio::spawn`.
    pub async fn from_env() -> Self {
        let opted_out = std::env::var("FBUILD_ZCCACHE_EMBEDDED")
            .map(|v| v == "0")
            .unwrap_or(false);

        if opted_out {
            tracing::info!(
                "zccache backend: wrapped (FBUILD_ZCCACHE_EMBEDDED=0 opt-out)"
            );
            return Self::Wrapped;
        }

        #[cfg(feature = "embedded")]
        {
            match FbuildZccacheService::start().await {
                Ok(svc) => {
                    tracing::info!(
                        "zccache backend: embedded (default; cache_root={})",
                        svc.cache_root().display()
                    );
                    return Self::Embedded(Arc::new(svc));
                }
                Err(EmbeddedServiceError::Start(err)) => {
                    tracing::warn!(
                        "embedded zccache service failed to start: {err}; \
                         falling back to wrapped"
                    );
                }
                Err(other) => {
                    tracing::warn!(
                        "embedded zccache service start raised an \
                         unexpected error: {other}; falling back to wrapped"
                    );
                }
            }
        }
        #[cfg(not(feature = "embedded"))]
        {
            tracing::info!(
                "zccache backend: wrapped (binary built without \
                 `embedded` Cargo feature)"
            );
        }

        Self::Wrapped
    }
}

/// Process-wide global compile backend, installed once at daemon
/// startup by [`install_global`].
///
/// Phase 2 of FastLED/fbuild#789 (#791): per-compile call sites
/// (`compile_source` and friends) live in `fbuild-build` and don't
/// have a handle to `DaemonContext`. The global handle bridges that
/// gap without touching every signature. The handle is set once
/// from the daemon's `#[tokio::main]` before any compile fires, and
/// read lock-free thereafter.
static GLOBAL: OnceLock<GlobalCompileBackend> = OnceLock::new();

/// Bundle of `CompileBackend` + the tokio runtime handle needed to
/// dispatch async compiles from synchronous call sites.
///
/// The runtime is only required for `CompileBackend::Embedded`; for
/// `Wrapped` it's `None`. Captured at [`install_global`] time
/// because `tokio::runtime::Handle::current()` only works from
/// inside a tokio runtime — `compile_source` is called from rayon
/// workers and arbitrary threads that don't have one.
pub struct GlobalCompileBackend {
    pub backend: CompileBackend,
    #[cfg(feature = "embedded")]
    runtime: Option<tokio::runtime::Handle>,
}

impl GlobalCompileBackend {
    /// Runtime handle suitable for `Handle::block_on(...)`. Always
    /// `Some` when `backend` is `Embedded`, `None` otherwise.
    #[cfg(feature = "embedded")]
    pub fn runtime(&self) -> Option<&tokio::runtime::Handle> {
        self.runtime.as_ref()
    }
}

/// Install the process-wide compile backend.
///
/// Must be called from inside the daemon's tokio runtime when
/// `backend` is `Embedded` — captures the current runtime handle so
/// later synchronous `compile_blocking` calls can `block_on(...)`
/// the async dispatch.
///
/// Idempotent on second-call (logs a warning, second call is a
/// no-op). Tests that exercise multiple backends in one process
/// must instead call into the type-level constructors directly —
/// the global is for the daemon's single-resolution-at-startup
/// case.
pub fn install_global(backend: CompileBackend) {
    #[cfg(feature = "embedded")]
    let runtime = if matches!(backend, CompileBackend::Embedded(_)) {
        Some(tokio::runtime::Handle::current())
    } else {
        None
    };
    let global = GlobalCompileBackend {
        backend,
        #[cfg(feature = "embedded")]
        runtime,
    };
    if GLOBAL.set(global).is_err() {
        tracing::warn!(
            "compile_backend::install_global called more than once; second call ignored"
        );
    }
}

/// Read the process-wide compile backend, if installed.
///
/// `None` means "no global resolved yet" — call sites should
/// behave as if the backend were [`CompileBackend::Wrapped`].
pub fn get_global() -> Option<&'static GlobalCompileBackend> {
    GLOBAL.get()
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

    /// Phase 4 (FastLED/fbuild#793): `FBUILD_ZCCACHE_EMBEDDED=0` is
    /// the explicit opt-out and forces `Wrapped` regardless of the
    /// `embedded` feature flag.
    #[tokio::test(flavor = "current_thread")]
    async fn from_env_returns_wrapped_when_var_is_zero() {
        let prior = std::env::var_os("FBUILD_ZCCACHE_EMBEDDED");
        unsafe { std::env::set_var("FBUILD_ZCCACHE_EMBEDDED", "0") };
        let backend = CompileBackend::from_env().await;
        assert_eq!(backend.name(), "wrapped");
        match prior {
            Some(v) => unsafe { std::env::set_var("FBUILD_ZCCACHE_EMBEDDED", v) },
            None => unsafe { std::env::remove_var("FBUILD_ZCCACHE_EMBEDDED") },
        }
    }

    /// Without the `embedded` Cargo feature, `from_env` always
    /// returns `Wrapped`. Phase 4 only flips the *default* — if the
    /// library wasn't compiled in, there's no embedded path to take.
    #[cfg(not(feature = "embedded"))]
    #[tokio::test(flavor = "current_thread")]
    async fn from_env_returns_wrapped_when_feature_off() {
        let prior = std::env::var_os("FBUILD_ZCCACHE_EMBEDDED");
        unsafe { std::env::remove_var("FBUILD_ZCCACHE_EMBEDDED") };
        let backend = CompileBackend::from_env().await;
        assert_eq!(backend.name(), "wrapped");
        if let Some(v) = prior {
            unsafe { std::env::set_var("FBUILD_ZCCACHE_EMBEDDED", v) };
        }
    }
}
