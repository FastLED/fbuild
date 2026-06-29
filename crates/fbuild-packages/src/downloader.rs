//! Async HTTP file downloader with SHA256 checksum verification.
//!
//! Uses reqwest async client for parallel downloads. Supports streaming
//! downloads with progress reporting for large files.

use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fbuild_core::{FbuildError, Result};
use sha2::{Digest, Sha256};

use crate::http;

/// Number of GET attempts before giving up on a transient failure.
/// One initial attempt + two retries. Worst-case wall time at the
/// default backoff schedule is ~4 s of sleep before the third
/// attempt — barely registers on a healthy run, large enough to ride
/// out the `dl.registry.platformio.org` hiccups we keep seeing in the
/// nightly STM32 acceptance gate.
const MAX_ATTEMPTS: u32 = 3;

/// Per-attempt backoff sleeps: 1 s before attempt 2, 3 s before
/// attempt 3.
const RETRY_BACKOFFS: &[Duration] = &[Duration::from_secs(1), Duration::from_secs(3)];

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
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        match http::client().get(url).send().await {
            Ok(response) => {
                let status = response.status();
                if !status.is_success() {
                    // 4xx is deterministic, 5xx is retryable.
                    if status.is_server_error() && attempt < MAX_ATTEMPTS {
                        let sleep = RETRY_BACKOFFS
                            .get(attempt as usize - 1)
                            .copied()
                            .unwrap_or(Duration::from_secs(5));
                        tracing::warn!(
                            "download {}: HTTP {} on attempt {}/{}, retrying after {:?}",
                            url,
                            status,
                            attempt,
                            MAX_ATTEMPTS,
                            sleep
                        );
                        tokio::time::sleep(sleep).await;
                        continue;
                    }
                    return Err(FbuildError::PackageError(format!(
                        "download failed for {}: HTTP {}",
                        url, status
                    )));
                }
                return response.bytes().await.map(|b| b.to_vec()).map_err(|e| {
                    FbuildError::PackageError(format!("failed to read response body: {}", e))
                });
            }
            Err(e) => {
                if is_transient(&e) && attempt < MAX_ATTEMPTS {
                    let sleep = RETRY_BACKOFFS
                        .get(attempt as usize - 1)
                        .copied()
                        .unwrap_or(Duration::from_secs(5));
                    tracing::warn!(
                        "download {}: transient error on attempt {}/{} ({}), retrying after {:?}",
                        url,
                        attempt,
                        MAX_ATTEMPTS,
                        e,
                        sleep
                    );
                    tokio::time::sleep(sleep).await;
                    continue;
                }
                return Err(FbuildError::PackageError(format!(
                    "failed to download {}: {}",
                    url, e
                )));
            }
        }
    }
}

/// GET `url` and return the `Response` for streaming, retrying
/// transient failures on the initial fetch. Once the response body
/// has started streaming, errors are terminal (we don't restart
/// large downloads from byte 0 — that's a bigger lever than the
/// transient `dl.registry.platformio.org` errors call for).
async fn open_with_retry(url: &str) -> Result<reqwest::Response> {
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        match http::client().get(url).send().await {
            Ok(response) => {
                let status = response.status();
                if status.is_success() {
                    return Ok(response);
                }
                if status.is_server_error() && attempt < MAX_ATTEMPTS {
                    let sleep = RETRY_BACKOFFS
                        .get(attempt as usize - 1)
                        .copied()
                        .unwrap_or(Duration::from_secs(5));
                    tracing::warn!(
                        "download {}: HTTP {} on attempt {}/{}, retrying after {:?}",
                        url,
                        status,
                        attempt,
                        MAX_ATTEMPTS,
                        sleep
                    );
                    tokio::time::sleep(sleep).await;
                    continue;
                }
                return Err(FbuildError::PackageError(format!(
                    "download failed for {}: HTTP {}",
                    url, status
                )));
            }
            Err(e) => {
                if is_transient(&e) && attempt < MAX_ATTEMPTS {
                    let sleep = RETRY_BACKOFFS
                        .get(attempt as usize - 1)
                        .copied()
                        .unwrap_or(Duration::from_secs(5));
                    tracing::warn!(
                        "download {}: transient error on attempt {}/{} ({}), retrying after {:?}",
                        url,
                        attempt,
                        MAX_ATTEMPTS,
                        e,
                        sleep
                    );
                    tokio::time::sleep(sleep).await;
                    continue;
                }
                return Err(FbuildError::PackageError(format!(
                    "failed to download {}: {}",
                    url, e
                )));
            }
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
    let filename = url.rsplit('/').next().unwrap_or("download").to_string();
    let dest_path = dest_dir.join(&filename);

    let response = open_with_retry(url).await?;

    let total_bytes = response.content_length();
    let mut downloaded: u64 = 0;
    let mut buf = Vec::with_capacity(total_bytes.unwrap_or(8 * 1024 * 1024) as usize);
    let mut last_report = Instant::now();
    let mut last_pct: u32 = 0;

    let mut stream = response;
    while let Some(chunk) = stream
        .chunk()
        .await
        .map_err(|e| FbuildError::PackageError(format!("failed to read response body: {}", e)))?
    {
        buf.extend_from_slice(&chunk);
        downloaded += chunk.len() as u64;

        let elapsed = last_report.elapsed().as_secs();
        let current_pct = total_bytes
            .map(|t| {
                if t > 0 {
                    (downloaded as f64 / t as f64 * 100.0) as u32
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

    tokio::fs::write(&dest_path, &buf).await.map_err(|e| {
        FbuildError::PackageError(format!(
            "failed to write downloaded file to {}: {}",
            dest_path.display(),
            e
        ))
    })?;

    tracing::info!("downloaded {} ({} bytes)", filename, downloaded);
    Ok(dest_path)
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
    use tempfile::NamedTempFile;

    #[test]
    fn test_verify_checksum_valid() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"hello world").unwrap();
        f.flush().unwrap();

        // SHA256 of "hello world"
        let expected = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        verify_checksum(f.path(), expected).unwrap();
    }

    #[test]
    fn test_verify_checksum_invalid() {
        let mut f = NamedTempFile::new().unwrap();
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
                let resp = {
                    let mut guard = responses.lock().unwrap();
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
        port
    }

    /// #205 nightly STM32 acceptance gate started flaking on
    /// `dl.registry.platformio.org` transient errors. A 5xx must
    /// trigger a retry, and the retry must succeed.
    #[tokio::test]
    async fn get_with_retry_retries_on_5xx() {
        let responses = std::sync::Arc::new(std::sync::Mutex::new(vec![
            "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello",
        ]));
        let port = run_flaky_server(responses.clone()).await;
        let url = format!("http://127.0.0.1:{port}/file");
        let bytes = get_with_retry(&url).await.expect("retry should succeed");
        assert_eq!(bytes, b"hello");
    }

    /// 4xx is deterministic — it must NOT retry. The test queues a
    /// single 404; if the implementation retried we'd hit the server's
    /// empty-queue branch and the test would hang or panic.
    #[tokio::test]
    async fn get_with_retry_does_not_retry_on_4xx() {
        let responses = std::sync::Arc::new(std::sync::Mutex::new(vec![
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n",
        ]));
        let port = run_flaky_server(responses.clone()).await;
        let url = format!("http://127.0.0.1:{port}/missing");
        let err = get_with_retry(&url).await.expect_err("should error");
        assert!(
            err.to_string().contains("404"),
            "expected 404 in error, got: {err}"
        );
    }

    /// Repeated 5xx exhausts the budget and surfaces the last
    /// response.
    #[tokio::test]
    async fn get_with_retry_gives_up_after_max_attempts() {
        let responses = std::sync::Arc::new(std::sync::Mutex::new(vec![
            "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n",
            "HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n",
            "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n",
        ]));
        let port = run_flaky_server(responses.clone()).await;
        let url = format!("http://127.0.0.1:{port}/file");
        let err = get_with_retry(&url).await.expect_err("should give up");
        // Last attempt was a 503; that's what gets surfaced.
        assert!(
            err.to_string().contains("503"),
            "expected last-attempt 503 in error, got: {err}"
        );
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
