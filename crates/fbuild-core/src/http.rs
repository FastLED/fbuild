//! Shared HTTP client bridge.
//!
//! FastLED/fbuild#844 (bridge sweep) hoists `fbuild_packages::http::client()`
//! here so every reqwest construction in the workspace has one source of
//! truth. Lives in `fbuild-core` because every crate already depends on
//! `fbuild-core` (it's the bottom of the dependency graph). The matching
//! `ban_bare_reqwest` dylint forbids direct `reqwest::Client::new()` /
//! `reqwest::get` / `reqwest::ClientBuilder` outside this module.
//!
//! ## API surface
//!
//! - [`client()`] — process-shared async [`reqwest::Client`] (300s total,
//!   30s connect). Use this for every default-timeout call.
//! - [`client_with_timeout`] — per-call total-timeout override. Allocates
//!   a fresh client; do not call in a tight loop.
//! - [`blocking_client`] — for the rare OS-thread case (e.g. the
//!   `port_scan` worker pool that intentionally isolates reqwest off the
//!   tokio reactor).
//!
//! ## Timeouts
//!
//! Defaults are long enough for slow CDN downloads of multi-MB toolchain
//! archives and short enough that a wedged server doesn't pin a
//! fbuild-daemon worker indefinitely.

use std::sync::OnceLock;
use std::time::Duration;

use reqwest::Client;

/// Default per-request timeout. 5 minutes — sized for toolchain archive
/// downloads.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

/// Default connect timeout. 30 seconds — tight enough to surface DNS /
/// network breakage quickly, loose enough that a slow handshake doesn't
/// kill a download.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

static CLIENT: OnceLock<Client> = OnceLock::new();

/// Process-shared async [`reqwest::Client`]. Lazily built on first call.
///
/// All HTTP traffic in fbuild flows through this client unless a per-call
/// timeout override is required (use [`client_with_timeout`] for that).
pub fn client() -> &'static Client {
    CLIENT.get_or_init(|| {
        Client::builder()
            .timeout(DEFAULT_TIMEOUT)
            .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
            .build()
            .expect("reqwest client builder should never fail with these settings")
    })
}

/// Build a client with a custom total timeout (for callers that need a
/// tighter deadline, e.g. a registry ping that should fail fast).
///
/// Allocates a fresh client per call. Do not invoke in a tight loop —
/// reuse the returned client if multiple requests share the same
/// deadline.
pub fn client_with_timeout(total: Duration) -> Client {
    Client::builder()
        .timeout(total)
        .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
        .build()
        .expect("reqwest client builder should never fail with valid settings")
}

/// Build a blocking [`reqwest::blocking::Client`] for the rare OS-thread
/// case. Used by `port_scan` so reqwest's blocking machinery stays off
/// the tokio reactor.
///
/// Most callers should use [`client()`] (async) instead. The blocking
/// surface is intentionally narrow — if you find yourself reaching for
/// it inside an `async fn`, you almost certainly want [`client()`].
pub fn blocking_client(total: Duration) -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(total)
        .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
        .build()
        .expect("reqwest blocking client builder should never fail with valid settings")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shared client is stable across calls — same pointer.
    #[test]
    fn client_is_shared() {
        let a = client();
        let b = client();
        assert!(std::ptr::eq(a, b));
    }

    /// `client_with_timeout` returns a freshly-built client.
    #[test]
    fn client_with_timeout_builds() {
        let _c = client_with_timeout(Duration::from_secs(10));
    }

    /// `blocking_client` builds successfully — the OS-thread path.
    #[test]
    fn blocking_client_builds() {
        let _c = blocking_client(Duration::from_secs(10));
    }
}
