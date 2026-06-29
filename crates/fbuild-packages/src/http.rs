//! Shared async `reqwest::Client` with configured timeouts.
//!
//! FastLED/fbuild#813 (async migration) + #805 (timeout audit):
//! every HTTP call in fbuild-packages goes through this client.
//! Tokio-console sees the I/O because the underlying tokio
//! reactor is the daemon's. Timeouts default to safe values; per-
//! call overrides via `client_with_timeout(...)`.

use std::sync::OnceLock;
use std::time::Duration;

use reqwest::Client;

/// Default per-request timeout. Long enough for slow CDN downloads
/// of multi-MB toolchain archives, short enough that a wedged
/// server doesn't pin a fbuild-daemon worker indefinitely.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300); // 5 min
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

static CLIENT: OnceLock<Client> = OnceLock::new();

/// Get the shared client. Lazily built on first call.
pub fn client() -> &'static Client {
    CLIENT.get_or_init(|| {
        Client::builder()
            .timeout(DEFAULT_TIMEOUT)
            .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
            .build()
            .expect("reqwest client builder should never fail with these settings")
    })
}

/// Build a client with a custom total timeout (for callers that
/// need a tighter deadline, e.g. a registry ping that should
/// fail fast).
pub fn client_with_timeout(total: Duration) -> Client {
    Client::builder()
        .timeout(total)
        .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
        .build()
        .expect("reqwest client builder should never fail with valid settings")
}
