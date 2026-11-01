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

/// User agent sent by every shared client.
///
/// `dl.registry.platformio.org` now answers anything that does not identify
/// itself as PlatformIO Core with `403 This download method is not
/// supported. Please use PlatformIO Core to install packages.` The check is a
/// prefix test on `PlatformIO/`, so a cold toolchain download failed for every
/// board whose archive was not already cached (FastLED/fbuild#1783). fbuild
/// still names itself in the comment so registry-side logs are not lying.
pub fn registry_user_agent() -> String {
    format!("PlatformIO/6.2.0 (fbuild/{})", env!("CARGO_PKG_VERSION"))
}

static CLIENT: OnceLock<Client> = OnceLock::new();

/// Process-shared async [`reqwest::Client`]. Lazily built on first call.
///
/// All HTTP traffic in fbuild flows through this client unless a per-call
/// timeout override is required (use [`client_with_timeout`] for that).
pub fn client() -> &'static Client {
    CLIENT.get_or_init(|| {
        Client::builder()
            .user_agent(registry_user_agent())
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
        .user_agent(registry_user_agent())
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
        .user_agent(registry_user_agent())
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

    /// The registry only redirects downloads to agents whose UA starts with
    /// `PlatformIO/`; anything else is a 403 (FastLED/fbuild#1783).
    #[test]
    fn registry_user_agent_identifies_as_platformio() {
        let ua = registry_user_agent();
        assert!(
            ua.starts_with("PlatformIO/"),
            "UA must satisfy the registry gate: {ua}"
        );
        assert!(
            ua.contains(&format!("fbuild/{}", env!("CARGO_PKG_VERSION"))),
            "UA must name fbuild: {ua}"
        );
    }

    /// `blocking_client` builds successfully — the OS-thread path.
    #[test]
    fn blocking_client_builds() {
        let _c = blocking_client(Duration::from_secs(10));
    }

    /// Minimal response: enough for reqwest to consider the exchange
    /// complete so `send()` resolves and we can inspect what it sent.
    const MIN_RESPONSE: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";

    /// Pull the `User-Agent` value out of a raw HTTP request head. Header
    /// names are case-insensitive per RFC 9110, hence the ASCII fold.
    fn user_agent_of(head: &str) -> Option<&str> {
        head.lines()
            .find(|line| line.to_ascii_lowercase().starts_with("user-agent:"))
            .map(|line| line["user-agent:".len()..].trim())
    }

    /// Assert a captured request head carries a gate-satisfying UA.
    ///
    /// This is the assertion that guards the outage. The string-only test
    /// above cannot see a builder that forgot `.user_agent(...)`; this can,
    /// because it reads what actually went onto the socket.
    fn assert_passes_registry_gate(head: &str, who: &str) {
        let ua = user_agent_of(head)
            .unwrap_or_else(|| panic!("{who} sent no User-Agent header; head was:\n{head}"));
        assert!(
            ua.starts_with("PlatformIO/"),
            "{who} sent User-Agent {ua:?}; dl.registry.platformio.org answers \
             anything not starting with `PlatformIO/` with 403 \
             (FastLED/fbuild#1783)"
        );
        assert_eq!(ua, registry_user_agent(), "{who} must send the shared UA");
    }

    /// Read one HTTP request head (through the blank line) off a stream.
    async fn read_head(stream: &mut tokio::net::TcpStream) -> String {
        use tokio::io::AsyncReadExt;
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
            match stream.read(&mut chunk).await.expect("read request") {
                0 => break,
                n => buf.extend_from_slice(&chunk[..n]),
            }
        }
        String::from_utf8_lossy(&buf).into_owned()
    }

    /// Serve exactly one request on loopback; hand back its head.
    async fn capture_one_request_head() -> (std::net::SocketAddr, tokio::task::JoinHandle<String>) {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let address = listener.local_addr().expect("local addr");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let head = read_head(&mut stream).await;
            stream.write_all(MIN_RESPONSE).await.expect("respond");
            stream.flush().await.expect("flush");
            head
        });
        (address, server)
    }

    /// The shared async client really puts the UA on the wire.
    ///
    /// Loopback only — no network egress, so this runs wherever CI runs.
    #[tokio::test]
    async fn shared_client_sends_platformio_user_agent_on_the_wire() {
        let (address, server) = capture_one_request_head().await;
        let response = client()
            .get(format!("http://{address}/download/toolchain.tar.gz"))
            .send()
            .await
            .expect("loopback GET should succeed");
        assert!(response.status().is_success());
        assert_passes_registry_gate(&server.await.expect("server task"), "client()");
    }

    /// Same guarantee for the per-call-timeout variant.
    #[tokio::test]
    async fn client_with_timeout_sends_platformio_user_agent_on_the_wire() {
        let (address, server) = capture_one_request_head().await;
        client_with_timeout(Duration::from_secs(10))
            .get(format!("http://{address}/"))
            .send()
            .await
            .expect("loopback GET should succeed");
        assert_passes_registry_gate(&server.await.expect("server task"), "client_with_timeout()");
    }

    /// And for the blocking client. Deliberately a plain `#[test]`: reqwest's
    /// blocking surface refuses to run inside a tokio runtime, which is the
    /// very reason this variant exists.
    #[test]
    fn blocking_client_sends_platformio_user_agent_on_the_wire() {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let address = listener.local_addr().expect("local addr");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = Vec::new();
            let mut chunk = [0u8; 1024];
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut chunk).expect("read request") {
                    0 => break,
                    n => buf.extend_from_slice(&chunk[..n]),
                }
            }
            stream.write_all(MIN_RESPONSE).expect("respond");
            stream.flush().expect("flush");
            String::from_utf8_lossy(&buf).into_owned()
        });

        blocking_client(Duration::from_secs(10))
            .get(format!("http://{address}/"))
            .send()
            .expect("loopback GET should succeed");
        assert_passes_registry_gate(&server.join().expect("server thread"), "blocking_client()");
    }

    /// Live proof against the real CDN, for a human debugging a recurrence.
    /// Ignored: CI has no guaranteed network egress and this asserts the
    /// behaviour of a third-party service.
    #[tokio::test]
    #[ignore = "requires network egress to dl.registry.platformio.org"]
    async fn live_registry_accepts_our_user_agent() {
        let response = client()
            .get(concat!(
                "https://dl.registry.platformio.org/download/platformio/tool/",
                "framework-arduino-avr/5.4.0/framework-arduino-avr-5.4.0.tar.gz"
            ))
            .header("Range", "bytes=0-1023")
            .send()
            .await
            .expect("registry GET");
        assert!(
            response.status().is_success(),
            "registry rejected UA {:?} with {}",
            registry_user_agent(),
            response.status()
        );
    }
}
