//! Async HTTP file downloader with SHA256 checksum verification.
//!
//! Uses reqwest async client for parallel downloads. Supports streaming
//! downloads with progress reporting for large files.

use std::fmt::{Display, Formatter, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fbuild_core::{FbuildError, Result};
use sha2::{Digest, Sha256};

use crate::http;

/// Number of complete GET attempts before giving up on a transient failure.
/// The retry boundary covers both request setup and response-body transfer.
const MAX_ATTEMPTS: u32 = 5;

/// Exponential sleeps after failed attempts 1 through 4.
const RETRY_BACKOFFS: &[Duration] = &[
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
];

/// Per-chunk deadline for streaming downloads. A stall fails the current
/// attempt and is retried under the same budget as other transient failures.
const CHUNK_READ_TIMEOUT: Duration = Duration::from_secs(60);

/// Classify a `reqwest::Error` as worth retrying — anything that
/// could plausibly succeed on a retry (connect timeout, request /
/// body recv error, server-side 5xx). Deterministic-looking failures
/// (URL parse, 4xx) are NOT retried; they'd just waste time.
fn is_transient(err: &reqwest::Error) -> bool {
    if err.is_timeout() || err.is_connect() || err.is_request() || err.is_body() {
        return true;
    }
    if let Some(status) = err.status() {
        return status.is_server_error();
    }
    // No HTTP status, not classified above → most likely a
    // network-stack transient (DNS, TLS handshake). Retry.
    true
}

#[derive(Debug)]
enum DownloadAttemptError {
    Request(reqwest::Error),
    HttpStatus(reqwest::StatusCode),
    Body(reqwest::Error),
    BodyStalled { filename: String },
}

impl DownloadAttemptError {
    fn is_retryable(&self) -> bool {
        match self {
            Self::Request(error) | Self::Body(error) => is_transient(error),
            Self::HttpStatus(status) => status.is_server_error(),
            Self::BodyStalled { .. } => true,
        }
    }

    fn into_fbuild_error(self, url: &str) -> FbuildError {
        match self {
            Self::Request(error) => {
                FbuildError::PackageError(format!("failed to download {}: {}", url, error))
            }
            Self::HttpStatus(status) => {
                FbuildError::PackageError(format!("download failed for {}: HTTP {}", url, status))
            }
            Self::Body(error) => {
                FbuildError::PackageError(format!("failed to read response body: {}", error))
            }
            Self::BodyStalled { filename } => FbuildError::PackageError(format!(
                "body read stalled > {}s while downloading {}",
                CHUNK_READ_TIMEOUT.as_secs(),
                filename
            )),
        }
    }
}

impl Display for DownloadAttemptError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Request(error) => write!(f, "request error: {error}"),
            Self::HttpStatus(status) => write!(f, "HTTP {status}"),
            Self::Body(error) => write!(f, "response body error: {error}"),
            Self::BodyStalled { filename } => write!(
                f,
                "body read stalled > {}s while downloading {}",
                CHUNK_READ_TIMEOUT.as_secs(),
                filename
            ),
        }
    }
}

async fn open_attempt(
    client: &reqwest::Client,
    url: &str,
) -> std::result::Result<reqwest::Response, DownloadAttemptError> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(DownloadAttemptError::Request)?;
    let status = response.status();
    if status.is_success() {
        Ok(response)
    } else {
        Err(DownloadAttemptError::HttpStatus(status))
    }
}

fn retry_backoff(attempt: u32) -> Duration {
    debug_assert!((1..MAX_ATTEMPTS).contains(&attempt));
    RETRY_BACKOFFS[(attempt - 1) as usize]
}

async fn wait_before_retry(url: &str, attempt: u32, error: &DownloadAttemptError) {
    let delay = retry_backoff(attempt);
    tracing::warn!(
        "download {}: {} on attempt {}/{}, retrying after {:?}",
        url,
        error,
        attempt,
        MAX_ATTEMPTS,
        delay
    );
    tokio::time::sleep(delay).await;
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{:02x}", b);
            s
        })
}

/// Progress information for a download in progress.
#[derive(Debug, Clone)]
pub struct DownloadProgress {
    pub downloaded: u64,
    pub total_bytes: Option<u64>,
    pub filename: String,
}

impl DownloadProgress {
    /// Format a human-readable progress message.
    pub fn format_message(&self) -> String {
        let dl_mb = self.downloaded as f64 / (1024.0 * 1024.0);
        match self.total_bytes {
            Some(total) => {
                let total_mb = total as f64 / (1024.0 * 1024.0);
                let pct = if total > 0 {
                    (self.downloaded as f64 / total as f64 * 100.0) as u32
                } else {
                    0
                };
                format!(
                    "downloading {}: {:.0}/{:.0} MB ({}%)",
                    self.filename, dl_mb, total_mb, pct
                )
            }
            None => {
                format!("downloading {}: {:.0} MB", self.filename, dl_mb)
            }
        }
    }
}

/// Download a file from a URL into the destination directory (async).
///
/// Returns the path to the downloaded file. Uses buffered download (loads
/// entire response into memory). For large files with progress reporting,
/// use [`download_file_with_progress`].
pub async fn download_file(url: &str, dest_dir: &Path) -> Result<PathBuf> {
    let filename = url.rsplit('/').next().unwrap_or("download").to_string();
    let dest_path = dest_dir.join(&filename);

    let bytes = get_with_retry(url).await?;

    tokio::fs::write(&dest_path, &bytes).await.map_err(|e| {
        FbuildError::PackageError(format!(
            "failed to write downloaded file to {}: {}",
            dest_path.display(),
            e
        ))
    })?;

    tracing::info!("downloaded {} ({} bytes)", filename, bytes.len());
    Ok(dest_path)
}

/// GET `url` and return the body bytes, retrying transient failures
/// up to [`MAX_ATTEMPTS`] times with [`RETRY_BACKOFFS`] between
/// attempts. A non-2xx HTTP status is treated as a hard failure
/// (only server-side 5xx is retried).
async fn get_with_retry(url: &str) -> Result<Vec<u8>> {
    get_with_retry_using(http::client(), url).await
}

async fn get_with_retry_using(client: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        let result = match open_attempt(client, url).await {
            Ok(response) => response
                .bytes()
                .await
                .map(|bytes| bytes.to_vec())
                .map_err(DownloadAttemptError::Body),
            Err(error) => Err(error),
        };
        match result {
            Ok(bytes) => return Ok(bytes),
            Err(error) if error.is_retryable() && attempt < MAX_ATTEMPTS => {
                wait_before_retry(url, attempt, &error).await;
            }
            Err(error) => return Err(error.into_fbuild_error(url)),
        }
    }
}

/// Download a file with streaming progress reporting.
///
/// The `on_progress` callback is called periodically during the download
/// (every 15 seconds or every 10% progress, whichever comes first).
pub async fn download_file_with_progress(
    url: &str,
    dest_dir: &Path,
    on_progress: &mut dyn FnMut(&DownloadProgress),
) -> Result<PathBuf> {
    download_file_with_progress_using(http::client(), url, dest_dir, on_progress).await?;
    let filename = url.rsplit('/').next().unwrap_or("download");
    Ok(dest_dir.join(filename))
}

async fn download_file_with_progress_using(
    client: &reqwest::Client,
    url: &str,
    dest_dir: &Path,
    on_progress: &mut dyn FnMut(&DownloadProgress),
) -> Result<()> {
    let filename = url.rsplit('/').next().unwrap_or("download").to_string();
    let dest_path = dest_dir.join(&filename);

    let mut attempt: u32 = 0;
    let buf = loop {
        attempt += 1;
        let result: std::result::Result<Vec<u8>, DownloadAttemptError> = async {
            let response = open_attempt(client, url).await?;
            let total_bytes = response.content_length();
            let mut downloaded: u64 = 0;
            let mut attempt_buf =
                Vec::with_capacity(total_bytes.unwrap_or(8 * 1024 * 1024) as usize);
            let mut last_report = Instant::now();
            let mut last_pct: u32 = 0;
            let mut stream = response;
            // FastLED/fbuild#805 CRITICAL: per-chunk deadline. The shared
            // `http::client()` already enforces a 300 s total-request timeout,
            // but defense-in-depth — wrap each `chunk().await` in a 60 s
            // tokio timeout so a stalled mid-download fails *this* attempt
            // promptly instead of waiting out the 5 min total. This is what
            // the audit calls out specifically: streaming body reads have no
            // per-chunk wake-up signal otherwise.
            loop {
                let chunk = match tokio::time::timeout(CHUNK_READ_TIMEOUT, stream.chunk()).await {
                    Ok(Ok(Some(chunk))) => chunk,
                    Ok(Ok(None)) => break,
                    Ok(Err(error)) => return Err(DownloadAttemptError::Body(error)),
                    Err(_) => {
                        return Err(DownloadAttemptError::BodyStalled {
                            filename: filename.clone(),
                        })
                    }
                };
                attempt_buf.extend_from_slice(&chunk);
                downloaded += chunk.len() as u64;

                let elapsed = last_report.elapsed().as_secs();
                let current_pct = total_bytes
                    .map(|total| {
                        if total > 0 {
                            (downloaded as f64 / total as f64 * 100.0) as u32
                        } else {
                            0
                        }
                    })
                    .unwrap_or(0);
                let pct_jump = current_pct >= last_pct + 10;

                if elapsed >= 15 || pct_jump {
                    let progress = DownloadProgress {
                        downloaded,
                        total_bytes,
                        filename: filename.clone(),
                    };
                    on_progress(&progress);
                    last_report = Instant::now();
                    last_pct = current_pct;
                }
            }
            Ok(attempt_buf)
        }
        .await;

        match result {
            Ok(bytes) => break bytes,
            Err(error) if error.is_retryable() && attempt < MAX_ATTEMPTS => {
                wait_before_retry(url, attempt, &error).await;
            }
            Err(error) => return Err(error.into_fbuild_error(url)),
        }
    };

    tokio::fs::write(&dest_path, &buf).await.map_err(|e| {
        FbuildError::PackageError(format!(
            "failed to write downloaded file to {}: {}",
            dest_path.display(),
            e
        ))
    })?;

    tracing::info!("downloaded {} ({} bytes)", filename, buf.len());
    Ok(())
}

/// Download multiple files in parallel (async).
///
/// Returns paths to all downloaded files. Fails fast on first error.
pub async fn download_all(urls: &[(&str, &Path)]) -> Result<Vec<PathBuf>> {
    let mut handles = Vec::new();

    for &(url, dest_dir) in urls {
        let url = url.to_string();
        let dest_dir = dest_dir.to_path_buf();
        handles.push(tokio::spawn(
            async move { download_file(&url, &dest_dir).await },
        ));
    }

    let mut results = Vec::new();
    for handle in handles {
        let path = handle
            .await
            .map_err(|e| FbuildError::PackageError(format!("download task failed: {}", e)))??;
        results.push(path);
    }

    Ok(results)
}

/// Verify a file's SHA256 checksum.
pub fn verify_checksum(path: &Path, expected: &str) -> Result<()> {
    let data = std::fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&data);
    let result = hasher.finalize();
    let actual = hex_encode(&result);

    if actual != expected.to_lowercase() {
        return Err(FbuildError::PackageError(format!(
            "checksum mismatch for {}: expected {}, got {}",
            path.display(),
            expected,
            actual
        )));
    }

    Ok(())
}

/// Async version of verify_checksum (reads file with tokio).
pub async fn verify_checksum_async(path: &Path, expected: &str) -> Result<()> {
    let data = tokio::fs::read(path).await?;
    let mut hasher = Sha256::new();
    hasher.update(&data);
    let result = hasher.finalize();
    let actual = hex_encode(&result);

    if actual != expected.to_lowercase() {
        return Err(FbuildError::PackageError(format!(
            "checksum mismatch for {}: expected {}, got {}",
            path.display(),
            expected,
            actual
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
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
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("checksum mismatch"));
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

        tokio::spawn(async move {
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
                let _ =
                    tokio::time::timeout(Duration::from_millis(200), stream.read(&mut buf)).await;
                let _ = stream.write_all(resp.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });
        tokio::task::yield_now().await;
        port
    }

    async fn run_stalling_server(request_count: std::sync::Arc<AtomicUsize>) -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(pair) => pair,
                    Err(_) => break,
                };
                request_count.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut stream = stream;
                    let mut request = [0u8; 1024];
                    let _ =
                        tokio::time::timeout(Duration::from_millis(200), stream.read(&mut request))
                            .await;
                    let _ = stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\n",
                        )
                        .await;
                    tokio::time::sleep(Duration::from_secs(120)).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        tokio::task::yield_now().await;
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
    #[tokio::test(start_paused = true)]
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
    #[tokio::test(start_paused = true)]
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

    #[tokio::test(start_paused = true)]
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

    #[tokio::test(start_paused = true)]
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

        let err = get_with_retry_using(&test_client(), &url)
            .await
            .expect_err("the fifth truncated response should exhaust retries");

        assert!(
            err.to_string().contains("failed to read response body"),
            "expected final body error, got: {err}"
        );
        assert_eq!(request_count.load(Ordering::SeqCst), 5);
    }

    #[tokio::test(start_paused = true)]
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

    #[tokio::test(start_paused = true)]
    async fn streaming_download_stops_after_five_truncated_bodies_without_output() {
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

        let err =
            download_file_with_progress_using(&test_client(), &url, temp.path(), &mut progress)
                .await
                .expect_err("the fifth truncated response should exhaust retries");

        assert!(
            err.to_string().contains("failed to read response body"),
            "expected final body error, got: {err}"
        );
        assert_eq!(request_count.load(Ordering::SeqCst), 5);
        assert!(!temp.path().join("file").exists());
    }

    #[tokio::test(start_paused = true)]
    async fn streaming_download_retries_chunk_stalls_five_times_without_output() {
        let _guard = network_test_guard().await;
        let request_count = std::sync::Arc::new(AtomicUsize::new(0));
        let port = run_stalling_server(request_count.clone()).await;
        let url = format!("http://127.0.0.1:{port}/file");
        let temp = tempfile::TempDir::new().unwrap();
        let mut progress = |_progress: &DownloadProgress| {};

        let err =
            download_file_with_progress_using(&test_client(), &url, temp.path(), &mut progress)
                .await
                .expect_err("five chunk stalls should exhaust retries");

        assert!(
            err.to_string().contains("body read stalled > 60s"),
            "expected final chunk-stall error, got: {err}"
        );
        assert_eq!(request_count.load(Ordering::SeqCst), 5);
        assert!(!temp.path().join("file").exists());
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
}
