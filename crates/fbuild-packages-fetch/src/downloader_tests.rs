//! Tests for [`super`] — the retrying, resumable downloader.
//!
//! Split out of `downloader.rs` to keep that file under the workspace's
//! 1000-LOC limit; `compiler_tests.rs` is the same pattern.

use super::*;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::NamedTempFile;

static NETWORK_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn network_test_guard() -> tokio::sync::MutexGuard<'static, ()> {
    NETWORK_TEST_LOCK.lock().await
}

fn named_temp_file() -> NamedTempFile {
    NamedTempFile::new_in(fbuild_paths::temp_subdir(
        "fbuild-packages-downloader-tests",
    ))
    .unwrap()
}

fn test_client() -> reqwest::Client {
    fbuild_core::http::client_with_timeout(Duration::from_secs(300))
}

#[test]
fn test_verify_checksum_valid() {
    let mut f = named_temp_file();
    f.write_all(b"hello world").unwrap();
    f.flush().unwrap();

    // SHA256 of "hello world"
    let expected = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
    verify_checksum(f.path(), expected).unwrap();
}

#[test]
fn test_verify_checksum_invalid() {
    let mut f = named_temp_file();
    f.write_all(b"hello world").unwrap();
    f.flush().unwrap();

    let result = verify_checksum(
        f.path(),
        "0000000000000000000000000000000000000000000000000000000000000000",
    );
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("checksum mismatch")
    );
}

// ---- transient-retry tests ----

/// Stand up a tiny raw-TCP HTTP server on a loopback port. Reads
/// one request, drops the body, writes whatever 4-line HTTP
/// response the caller queued for that attempt, and closes the
/// connection. The caller pre-queues a Vec of responses, one per
/// attempt; the server pops the next one as each connection
/// comes in. Keeps the deps to tokio (already required).
async fn run_flaky_server(
    responses: std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>,
    request_count: std::sync::Arc<AtomicUsize>,
) -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();

    tokio::spawn(async move {
        // A bound listener is not necessarily being polled yet.  Make
        // the caller wait until this task has reached its accept loop so
        // a retry test cannot burn an attempt during task startup on a
        // loaded runner.
        let _ = ready_tx.send(());
        loop {
            let (mut stream, _) = match listener.accept().await {
                Ok(p) => p,
                Err(_) => break,
            };
            request_count.fetch_add(1, Ordering::SeqCst);
            let resp = {
                let mut guard = responses.lock().unwrap_or_else(|err| err.into_inner());
                if guard.is_empty() {
                    break;
                }
                guard.remove(0)
            };
            let mut buf = [0u8; 1024];
            // Read just the request headers — don't care about the
            // body for these tests.
            // The client under test always writes a request.  Do not use
            // a paused-clock timeout here: it races the retry backoff and
            // can make the mock emit a response before the request task
            // has been scheduled on macOS.
            let _ = stream.read(&mut buf).await;
            let _ = stream.write_all(resp.as_bytes()).await;
            let _ = stream.shutdown().await;
        }
    });
    ready_rx.await.expect("flaky test server task should start");
    port
}

/// How long `run_stalling_server` withholds the announced body. Only has
/// to comfortably exceed `FAST_RETRY_TIMING.chunk_read_timeout`.
const STALL_DURATION: Duration = Duration::from_secs(30);

async fn run_stalling_server(request_count: std::sync::Arc<AtomicUsize>) -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();

    tokio::spawn(async move {
        let _ = ready_tx.send(());
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(pair) => pair,
                Err(_) => break,
            };
            request_count.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut stream = stream;
                let mut request = [0u8; 1024];
                // See `run_flaky_server`: this test owns the client, so
                // waiting for its request is deterministic.
                let _ = stream.read(&mut request).await;
                let _ = stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\n")
                    .await;
                // Announce a body, then never send it. Just needs to
                // outlast the client's per-chunk deadline; the task is
                // dropped at runtime shutdown, so the test doesn't wait
                // on it (FastLED/fbuild#1222 — this runs in real time
                // now, not paused time).
                tokio::time::sleep(STALL_DURATION).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    ready_rx
        .await
        .expect("stalling test server task should start");
    port
}

fn truncated_response() -> &'static str {
    "HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\nshort"
}

fn complete_response() -> &'static str {
    "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello"
}

#[test]
fn retry_policy_is_five_attempts_with_exponential_backoff() {
    assert_eq!(MAX_ATTEMPTS, 5);
    assert_eq!(
        RETRY_BACKOFFS,
        &[
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_secs(4),
            Duration::from_secs(8),
        ]
    );
}

/// #205 nightly STM32 acceptance gate started flaking on
/// `dl.registry.platformio.org` transient errors. A 5xx must
/// trigger a retry, and the retry must succeed.
#[tokio::test]
async fn get_with_retry_retries_on_5xx() {
    let _guard = network_test_guard().await;
    let responses = std::sync::Arc::new(std::sync::Mutex::new(vec![
        "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        complete_response(),
    ]));
    let request_count = std::sync::Arc::new(AtomicUsize::new(0));
    let port = run_flaky_server(responses.clone(), request_count.clone()).await;
    let url = format!("http://127.0.0.1:{port}/file");
    let bytes = get_with_retry_using(&test_client(), &url)
        .await
        .expect("retry should succeed");
    assert_eq!(bytes, b"hello");
    assert_eq!(request_count.load(Ordering::SeqCst), 5);
}

/// 4xx is deterministic — it must NOT retry. The test queues a
/// single 404; if the implementation retried we'd hit the server's
/// empty-queue branch and the test would hang or panic.
#[tokio::test]
async fn get_with_retry_does_not_retry_on_4xx() {
    let _guard = network_test_guard().await;
    let responses = std::sync::Arc::new(std::sync::Mutex::new(vec![
        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    ]));
    let request_count = std::sync::Arc::new(AtomicUsize::new(0));
    let port = run_flaky_server(responses.clone(), request_count.clone()).await;
    let url = format!("http://127.0.0.1:{port}/missing");
    let err = get_with_retry_using(&test_client(), &url)
        .await
        .expect_err("should error");
    assert!(
        err.to_string().contains("404"),
        "expected 404 in error, got: {err}"
    );
    assert_eq!(request_count.load(Ordering::SeqCst), 1);
}

/// Repeated 5xx exhausts the budget and surfaces the last
/// response.
#[tokio::test]
async fn get_with_retry_gives_up_after_max_attempts() {
    let _guard = network_test_guard().await;
    let responses = std::sync::Arc::new(std::sync::Mutex::new(vec![
        "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        "HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        "HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    ]));
    let request_count = std::sync::Arc::new(AtomicUsize::new(0));
    let port = run_flaky_server(responses.clone(), request_count.clone()).await;
    let url = format!("http://127.0.0.1:{port}/file");
    let err = get_with_retry_using(&test_client(), &url)
        .await
        .expect_err("should give up");
    // Last attempt was a 503; that's what gets surfaced.
    assert!(
        err.to_string().contains("503"),
        "expected last-attempt 503 in error, got: {err}"
    );
    assert_eq!(request_count.load(Ordering::SeqCst), 5);
}

#[tokio::test]
async fn get_with_retry_retries_truncated_bodies_until_attempt_five() {
    let _guard = network_test_guard().await;
    let responses = std::sync::Arc::new(std::sync::Mutex::new(vec![
        truncated_response(),
        truncated_response(),
        truncated_response(),
        truncated_response(),
        complete_response(),
    ]));
    let request_count = std::sync::Arc::new(AtomicUsize::new(0));
    let port = run_flaky_server(responses, request_count.clone()).await;
    let url = format!("http://127.0.0.1:{port}/file");

    let bytes = get_with_retry_using(&test_client(), &url)
        .await
        .expect("the fifth complete response should succeed");

    assert_eq!(bytes, b"hello");
    assert_eq!(request_count.load(Ordering::SeqCst), 5);
}

#[tokio::test]
async fn get_with_retry_stops_after_five_truncated_bodies() {
    let _guard = network_test_guard().await;
    let responses = std::sync::Arc::new(std::sync::Mutex::new(vec![
        truncated_response(),
        truncated_response(),
        truncated_response(),
        truncated_response(),
        truncated_response(),
    ]));
    let request_count = std::sync::Arc::new(AtomicUsize::new(0));
    let port = run_flaky_server(responses, request_count.clone()).await;
    let url = format!("http://127.0.0.1:{port}/file");

    let _err = get_with_retry_using(&test_client(), &url)
        .await
        .expect_err("the fifth truncated response should exhaust retries");

    // The final transient can surface either while reqwest reads the
    // deliberately short body or while it opens that last connection.
    // The retry budget, rather than this transport-layer wording, is the
    // contract under test.
    assert_eq!(request_count.load(Ordering::SeqCst), 5);
}

#[tokio::test]
async fn streaming_download_retries_truncated_bodies_until_attempt_five() {
    let _guard = network_test_guard().await;
    let responses = std::sync::Arc::new(std::sync::Mutex::new(vec![
        truncated_response(),
        truncated_response(),
        truncated_response(),
        truncated_response(),
        complete_response(),
    ]));
    let request_count = std::sync::Arc::new(AtomicUsize::new(0));
    let port = run_flaky_server(responses, request_count.clone()).await;
    let url = format!("http://127.0.0.1:{port}/file");
    let temp = tempfile::TempDir::new().unwrap();
    let mut progress = |_progress: &DownloadProgress| {};

    download_file_with_progress_using(&test_client(), &url, temp.path(), &mut progress)
        .await
        .expect("the fifth complete response should succeed");

    assert_eq!(std::fs::read(temp.path().join("file")).unwrap(), b"hello");
    assert_eq!(request_count.load(Ordering::SeqCst), 5);
}

#[tokio::test]
async fn streaming_download_stops_after_five_stalled_attempts_without_output() {
    let _guard = network_test_guard().await;
    let responses = std::sync::Arc::new(std::sync::Mutex::new(vec![
        truncated_response(),
        truncated_response(),
        truncated_response(),
        truncated_response(),
        truncated_response(),
    ]));
    let request_count = std::sync::Arc::new(AtomicUsize::new(0));
    let port = run_flaky_server(responses, request_count.clone()).await;
    let url = format!("http://127.0.0.1:{port}/file");
    let temp = tempfile::TempDir::new().unwrap();
    let mut progress = |_progress: &DownloadProgress| {};

    let _err = download_file_with_progress_using(&test_client(), &url, temp.path(), &mut progress)
        .await
        .expect_err("the fifth truncated response should exhaust retries");

    // Six, not five, and the extra one is the point of
    // FastLED/fbuild#1370: the budget is now five attempts that make *no
    // progress*, not five attempts total. The first attempt advances the
    // part file from 0 to the truncation point, so it does not spend
    // budget; the five after it re-deliver the same prefix (this mock
    // ignores `Range`) and do. A download that keeps advancing is no
    // longer cut off at a fixed attempt count, which is what let a large
    // toolchain fail forever on a connection that could not carry it in
    // one stream.
    assert_eq!(request_count.load(Ordering::SeqCst), 6);
    assert!(!temp.path().join("file").exists());
    assert!(!temp.path().join("file.part").exists());
}

/// A server that drops the connection partway through the body, then
/// serves the remainder to a ranged retry.
///
/// This is the shape FastLED/fbuild#1370 reported: a 282 MB download that
/// died in the same 80-98 MB band every time. `honor_range = false`
/// models an origin that ignores `Range` and restarts the body, which is
/// legal and must not corrupt the partial file.
async fn run_resuming_server(
    body: &'static [u8],
    first_len: usize,
    honor_range: bool,
    request_count: std::sync::Arc<AtomicUsize>,
) -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();

    tokio::spawn(async move {
        let _ = ready_tx.send(());
        loop {
            let (mut stream, _) = match listener.accept().await {
                Ok(pair) => pair,
                Err(_) => break,
            };
            request_count.fetch_add(1, Ordering::SeqCst);

            let mut buf = [0u8; 2048];
            let read = stream.read(&mut buf).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..read]).to_string();
            let start = parse_request_range(&request);

            if honor_range && start > 0 && start < body.len() {
                let head = format!(
                    "HTTP/1.1 206 Partial Content\r\n\
                     Content-Length: {}\r\n\
                     Content-Range: bytes {}-{}/{}\r\n\
                     Accept-Ranges: bytes\r\n\r\n",
                    body.len() - start,
                    start,
                    body.len() - 1,
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes()).await;
                let _ = stream.write_all(&body[start..]).await;
            } else {
                // Announce the whole body but hang up after `first_len`.
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes()).await;
                let _ = stream.write_all(&body[..first_len]).await;
            }
            let _ = stream.shutdown().await;
        }
    });
    ready_rx.await.expect("resuming test server should start");
    port
}

/// Pull the start offset out of a `Range: bytes=N-` request header.
fn parse_request_range(request: &str) -> usize {
    request
        .lines()
        .find_map(|line| {
            let value = line
                .strip_prefix("Range:")
                .or_else(|| line.strip_prefix("range:"))?;
            let spec = value.trim().strip_prefix("bytes=")?;
            spec.split('-').next()?.trim().parse::<usize>().ok()
        })
        .unwrap_or(0)
}

const RESUME_BODY: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";

/// The fix for FastLED/fbuild#1370: a dropped connection costs only the
/// bytes it did not deliver.
///
/// Without resume this needs the server to send a complete body in one
/// attempt, which is exactly what the reporter's connection could not do.
/// With it, two attempts finish the file and the second one asks for the
/// remainder rather than starting over.
#[tokio::test]
async fn streaming_download_resumes_from_the_byte_offset_after_a_drop() {
    let _guard = network_test_guard().await;
    let request_count = std::sync::Arc::new(AtomicUsize::new(0));
    let port = run_resuming_server(RESUME_BODY, 10, true, request_count.clone()).await;
    let url = format!("http://127.0.0.1:{port}/file");
    let temp = tempfile::TempDir::new().unwrap();
    let mut seen: Vec<u64> = Vec::new();
    let mut progress = |p: &DownloadProgress| seen.push(p.downloaded);

    download_file_with_progress_using(&test_client(), &url, temp.path(), &mut progress)
        .await
        .expect("a ranged retry should finish the download");

    assert_eq!(
        std::fs::read(temp.path().join("file")).unwrap(),
        RESUME_BODY,
        "the resumed file must be byte-identical to the source"
    );
    assert_eq!(
        request_count.load(Ordering::SeqCst),
        2,
        "one dropped attempt plus one ranged resume"
    );
    assert!(
        !temp.path().join("file.part").exists(),
        "the part file must be renamed away, not left behind"
    );
    assert!(
        seen.iter().all(|d| *d <= RESUME_BODY.len() as u64),
        "reported progress must stay cumulative rather than restarting: {seen:?}"
    );
}

/// An origin that ignores `Range` must not corrupt the partial file, and
/// must still terminate rather than looping forever.
///
/// The second half of #1370 is that retries which make no progress cannot
/// converge. Here every attempt lands on the same byte, so the
/// no-progress budget is what stops it.
#[tokio::test]
async fn streaming_download_gives_up_when_the_server_ignores_range() {
    let _guard = network_test_guard().await;
    let request_count = std::sync::Arc::new(AtomicUsize::new(0));
    let port = run_resuming_server(RESUME_BODY, 10, false, request_count.clone()).await;
    let url = format!("http://127.0.0.1:{port}/file");
    let temp = tempfile::TempDir::new().unwrap();
    let mut progress = |_p: &DownloadProgress| {};

    let error = download_file_with_progress_using(&test_client(), &url, temp.path(), &mut progress)
        .await
        .expect_err("a server that never sends the tail must fail, not hang");

    let message = error.to_string();
    assert!(
        message.contains("byte offset"),
        "the failure must name the offset it stopped at, per #1370: {message}"
    );
    assert!(
        !temp.path().join("file").exists(),
        "no truncated archive may be left at the destination"
    );
    assert!(
        !temp.path().join("file.part").exists(),
        "the part file must be cleaned up on hard failure"
    );
    // One attempt makes progress (0 -> 10), then every later attempt
    // re-sends the same prefix, so the no-progress budget ends it.
    assert!(
        request_count.load(Ordering::SeqCst) >= MAX_STALLED_ATTEMPTS as usize,
        "should have spent the no-progress budget"
    );
}

#[test]
fn content_range_start_and_total_are_parsed() {
    // Exercised through the public shape rather than a Response, which
    // cannot be constructed here: the header grammar is the fragile part.
    assert_eq!(parse_request_range("Range: bytes=1234-\r\n"), 1234);
    assert_eq!(parse_request_range("range: bytes=0-\r\n"), 0);
    assert_eq!(parse_request_range("GET / HTTP/1.1\r\n"), 0);
}

/// Retry timings short enough to run in real time. This test previously
/// used `#[tokio::test(start_paused = true)]` against a real
/// `TcpListener`, which flaked on loaded macOS runners: paused time
/// auto-advances whenever the runtime looks idle, but socket readiness
/// comes from the OS reactor, so the clock could jump past a connection
/// that was about to reach `accept()` — leaving `request_count` short of
/// 5. Real durations remove the race entirely (FastLED/fbuild#1222).
const FAST_RETRY_TIMING: RetryTiming = RetryTiming {
    chunk_read_timeout: Duration::from_millis(150),
    backoffs: &[
        Duration::from_millis(10),
        Duration::from_millis(10),
        Duration::from_millis(10),
        Duration::from_millis(10),
    ],
};

#[tokio::test]
async fn streaming_download_retries_chunk_stalls_five_times_without_output() {
    let _guard = network_test_guard().await;
    let request_count = std::sync::Arc::new(AtomicUsize::new(0));
    let port = run_stalling_server(request_count.clone()).await;
    let url = format!("http://127.0.0.1:{port}/file");
    let temp = tempfile::TempDir::new().unwrap();
    let mut progress = |_progress: &DownloadProgress| {};

    let _err = download_file_with_progress_timed(
        &test_client(),
        &url,
        temp.path(),
        &mut progress,
        FAST_RETRY_TIMING,
    )
    .await
    .expect_err("five chunk stalls should exhaust retries");

    // A stalled body is retryable, as is a connection error while opening
    // a retry. The latter can legitimately be the final transient on a
    // busy platform, so the stable contract is exhausting all five
    // attempts without publishing an output.
    assert_eq!(request_count.load(Ordering::SeqCst), 5);
    assert!(!temp.path().join("file").exists());
}

/// The injected timings are a test seam, not a behavior change: the
/// production path must still carry the real constants.
#[test]
fn production_retry_timing_matches_the_constants() {
    assert_eq!(
        RetryTiming::PRODUCTION.chunk_read_timeout,
        CHUNK_READ_TIMEOUT
    );
    assert_eq!(RetryTiming::PRODUCTION.backoffs, RETRY_BACKOFFS);
    for attempt in 1..MAX_ATTEMPTS {
        assert_eq!(
            RetryTiming::PRODUCTION.backoff(attempt),
            RETRY_BACKOFFS[(attempt - 1) as usize]
        );
    }
}

#[test]
fn format_download_progress_with_total() {
    let p = DownloadProgress {
        downloaded: 50 * 1024 * 1024,
        total_bytes: Some(150 * 1024 * 1024),
        filename: "toolchain.tar.gz".into(),
    };
    let msg = p.format_message();
    assert!(msg.contains("50"), "msg: {msg}");
    assert!(msg.contains("150"), "msg: {msg}");
    assert!(msg.contains("33%"), "msg: {msg}");
}

#[test]
fn format_download_progress_without_total() {
    let p = DownloadProgress {
        downloaded: 5 * 1024 * 1024,
        total_bytes: None,
        filename: "library.zip".into(),
    };
    let msg = p.format_message();
    assert!(msg.contains("5"), "msg: {msg}");
    assert!(!msg.contains("%"), "msg: {msg}");
}

#[test]
fn format_download_progress_zero() {
    let p = DownloadProgress {
        downloaded: 0,
        total_bytes: Some(100 * 1024 * 1024),
        filename: "file.bin".into(),
    };
    let msg = p.format_message();
    assert!(msg.contains("0%"), "msg: {msg}");
}
