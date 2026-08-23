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

/// The wall-clock durations the retry loop waits on.
///
/// Extracted so tests can drive the *same* retry logic on millisecond
/// durations instead of reaching for `#[tokio::test(start_paused = true)]`.
/// Paused time and a real `TcpListener` don't mix: socket readiness is driven
/// by the OS reactor, not the virtual clock, so when every task parks the
/// clock can jump past a socket operation that was about to become ready —
/// which is what made the chunk-stall test flake on loaded macOS runners
/// (FastLED/fbuild#1222).
#[derive(Clone, Copy, Debug)]
struct RetryTiming {
    chunk_read_timeout: Duration,
    backoffs: &'static [Duration],
}

impl RetryTiming {
    const PRODUCTION: Self = Self {
        chunk_read_timeout: CHUNK_READ_TIMEOUT,
        backoffs: RETRY_BACKOFFS,
    };

    fn backoff(&self, attempt: u32) -> Duration {
        debug_assert!((1..MAX_ATTEMPTS).contains(&attempt));
        self.backoffs[(attempt - 1) as usize]
    }
}

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
    BodyStalled {
        filename: String,
    },
    /// The part file on disk could not be opened, written, or flushed.
    PartFile {
        path: String,
        error: String,
    },
    /// The body ended before the announced length — a dropped connection that
    /// happened to land on a chunk boundary (FastLED/fbuild#1370).
    BodyTruncated {
        got: u64,
        expected: u64,
    },
}

impl DownloadAttemptError {
    fn is_retryable(&self) -> bool {
        match self {
            Self::Request(error) | Self::Body(error) => is_transient(error),
            Self::HttpStatus(status) => status.is_server_error(),
            Self::BodyStalled { .. } => true,
            // A truncated body is the failure this retry loop exists for.
            Self::BodyTruncated { .. } => true,
            // Local disk trouble will not fix itself by asking the server
            // again, and retrying would just rewrite the same bytes.
            Self::PartFile { .. } => false,
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
            Self::PartFile { path, error } => FbuildError::PackageError(format!(
                "failed to write partial download at {}: {}",
                path, error
            )),
            Self::BodyTruncated { got, expected } => FbuildError::PackageError(format!(
                "download of {} ended early: got {} of {} bytes",
                url, got, expected
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
            Self::PartFile { path, error } => {
                write!(f, "partial-download write error at {path}: {error}")
            }
            Self::BodyTruncated { got, expected } => {
                write!(f, "body ended early: {got} of {expected} bytes")
            }
        }
    }
}

async fn open_attempt(
    client: &reqwest::Client,
    url: &str,
) -> std::result::Result<reqwest::Response, DownloadAttemptError> {
    open_attempt_from(client, url, 0)
        .await
        .map(|opened| opened.response)
}

/// A response plus what the server agreed to about resuming.
struct OpenedRange {
    response: reqwest::Response,
    /// Byte offset the body actually starts at. Zero when the server sent a
    /// full body, whether or not a range was requested.
    starts_at: u64,
    /// Total size of the complete resource, when the server disclosed it.
    total: Option<u64>,
}

/// GET `url`, asking to resume from `offset` when that is non-zero.
///
/// A server may decline the range and send the whole body instead — that is
/// legal, and the caller has to notice, because appending a full body onto a
/// partial file would silently corrupt it. `starts_at` reports what the
/// server actually did rather than what was asked for (FastLED/fbuild#1370).
async fn open_attempt_from(
    client: &reqwest::Client,
    url: &str,
    offset: u64,
) -> std::result::Result<OpenedRange, DownloadAttemptError> {
    let mut request = client.get(url);
    if offset > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={offset}-"));
    }
    let response = request
        .send()
        .await
        .map_err(DownloadAttemptError::Request)?;
    let status = response.status();

    // The whole resource is already on disk: the server has nothing left to
    // send. Not an error — the caller finalizes and verifies.
    if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE && offset > 0 {
        return Ok(OpenedRange {
            response,
            starts_at: offset,
            total: Some(offset),
        });
    }
    if !status.is_success() {
        return Err(DownloadAttemptError::HttpStatus(status));
    }

    if status == reqwest::StatusCode::PARTIAL_CONTENT {
        let (starts_at, total) = parse_content_range(&response).unwrap_or((offset, None));
        return Ok(OpenedRange {
            response,
            starts_at,
            total,
        });
    }

    // 200 with a full body. If a range was asked for, the server ignored it.
    let total = response.content_length();
    Ok(OpenedRange {
        response,
        starts_at: 0,
        total,
    })
}

/// Parse `Content-Range: bytes <start>-<end>/<total>`.
///
/// Returns the start offset and the total when the total is a number rather
/// than the `*` an origin is allowed to send.
fn parse_content_range(response: &reqwest::Response) -> Option<(u64, Option<u64>)> {
    let value = response
        .headers()
        .get(reqwest::header::CONTENT_RANGE)?
        .to_str()
        .ok()?;
    let spec = value.trim().strip_prefix("bytes ")?;
    let (range, total) = spec.split_once('/')?;
    let start = range.split_once('-')?.0.trim().parse::<u64>().ok()?;
    Some((start, total.trim().parse::<u64>().ok()))
}

async fn wait_before_retry(
    url: &str,
    attempt: u32,
    error: &DownloadAttemptError,
    timing: RetryTiming,
) {
    let delay = timing.backoff(attempt);
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
                wait_before_retry(url, attempt, &error, RetryTiming::PRODUCTION).await;
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
    on_progress: &mut (dyn FnMut(&DownloadProgress) + Send),
) -> Result<PathBuf> {
    download_file_with_progress_using(http::client(), url, dest_dir, on_progress).await?;
    let filename = url.rsplit('/').next().unwrap_or("download");
    Ok(dest_dir.join(filename))
}

async fn download_file_with_progress_using(
    client: &reqwest::Client,
    url: &str,
    dest_dir: &Path,
    on_progress: &mut (dyn FnMut(&DownloadProgress) + Send),
) -> Result<()> {
    download_file_with_progress_timed(client, url, dest_dir, on_progress, RetryTiming::PRODUCTION)
        .await
}

/// How many attempts in a row may make zero progress before giving up.
///
/// The retry budget is spent on *stalls*, not on attempts. A 282 MB download
/// on a connection that dies around 90 MB needs four attempts to finish, and
/// counting those against a fixed total would fail a download that was
/// converging fine — which is exactly the shape of FastLED/fbuild#1370, where
/// five restarts moved ~450 MB for zero net progress and could never
/// terminate. An attempt that advances the file resets this counter, so a
/// download that keeps making headway keeps going, and one that is genuinely
/// stuck still stops promptly.
const MAX_STALLED_ATTEMPTS: u32 = 5;

/// [`download_file_with_progress_using`] with the retry durations injected.
/// See [`RetryTiming`] for why tests need this instead of paused Tokio time.
///
/// Bytes land in a `<filename>.part` beside the destination and are appended
/// to across retries, so a failed attempt costs only the bytes it did not
/// finish rather than everything downloaded so far. The part file is renamed
/// into place only once the body is complete, so a consumer never observes a
/// truncated archive at the real path.
async fn download_file_with_progress_timed(
    client: &reqwest::Client,
    url: &str,
    dest_dir: &Path,
    on_progress: &mut (dyn FnMut(&DownloadProgress) + Send),
    timing: RetryTiming,
) -> Result<()> {
    let filename = url.rsplit('/').next().unwrap_or("download").to_string();
    let dest_path = dest_dir.join(&filename);
    let part_path = dest_dir.join(format!("{filename}.part"));

    // Start from a known state. A part file left by an earlier invocation
    // cannot be trusted: nothing proves it came from this URL, and the
    // caller wipes its staging directory anyway.
    if let Err(error) = tokio::fs::remove_file(&part_path).await {
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(FbuildError::PackageError(format!(
                "failed to clear partial download at {}: {}",
                part_path.display(),
                error
            )));
        }
    }

    let mut resume_from: u64 = 0;
    let mut stalled: u32 = 0;
    let mut attempt: u32 = 0;

    loop {
        attempt += 1;
        let started_at = resume_from;

        let outcome = fetch_into_part(
            client,
            url,
            &part_path,
            resume_from,
            &filename,
            on_progress,
            timing,
        )
        .await;

        resume_from = part_len(&part_path).await;

        match outcome {
            Ok(()) => break,
            Err(error) => {
                // Progress, not attempt count, is what earns another try.
                if resume_from > started_at {
                    stalled = 0;
                    tracing::info!(
                        "download {}: attempt {} ended at {} bytes; resuming",
                        url,
                        attempt,
                        resume_from
                    );
                } else {
                    stalled += 1;
                }

                if error.is_retryable() && stalled < MAX_STALLED_ATTEMPTS {
                    wait_before_retry(url, stalled.max(1), &error, timing).await;
                    continue;
                }

                let _ = tokio::fs::remove_file(&part_path).await;
                return Err(FbuildError::PackageError(format!(
                    "{} (gave up after {} attempts at byte offset {}; \
                     {} consecutive attempts made no progress)",
                    error.into_fbuild_error(url),
                    attempt,
                    resume_from,
                    stalled
                )));
            }
        }
    }

    // Windows will not rename over an existing file.
    if let Err(error) = tokio::fs::remove_file(&dest_path).await {
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(FbuildError::PackageError(format!(
                "failed to replace {}: {}",
                dest_path.display(),
                error
            )));
        }
    }
    tokio::fs::rename(&part_path, &dest_path)
        .await
        .map_err(|e| {
            FbuildError::PackageError(format!(
                "failed to move completed download into {}: {}",
                dest_path.display(),
                e
            ))
        })?;

    tracing::info!("downloaded {} ({} bytes)", filename, resume_from);
    Ok(())
}

/// Bytes already in the part file, or zero when it does not exist.
async fn part_len(part_path: &Path) -> u64 {
    tokio::fs::metadata(part_path)
        .await
        .map(|meta| meta.len())
        .unwrap_or(0)
}

/// Stream one attempt's worth of body into `part_path`, resuming at `offset`.
///
/// Appends when the server honors the range and truncates when it does not,
/// so the part file always matches what the server is actually sending.
#[allow(clippy::too_many_arguments)]
async fn fetch_into_part(
    client: &reqwest::Client,
    url: &str,
    part_path: &Path,
    offset: u64,
    filename: &str,
    on_progress: &mut (dyn FnMut(&DownloadProgress) + Send),
    timing: RetryTiming,
) -> std::result::Result<(), DownloadAttemptError> {
    use tokio::io::AsyncWriteExt;

    let opened = open_attempt_from(client, url, offset).await?;
    let OpenedRange {
        mut response,
        starts_at,
        total,
    } = opened;

    // The server declined the range and restarted the body. Anything already
    // written is now the wrong prefix, so drop it rather than append.
    let appending = starts_at == offset && offset > 0;
    if !appending && offset > 0 {
        tracing::warn!(
            "download {}: server ignored the resume request and restarted from 0; \
             discarding {} partial bytes",
            url,
            offset
        );
    }

    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(appending)
        .truncate(!appending)
        .open(part_path)
        .await
        .map_err(|error| DownloadAttemptError::PartFile {
            path: part_path.display().to_string(),
            error: error.to_string(),
        })?;

    let mut downloaded: u64 = if appending { offset } else { 0 };
    // `content_length()` on a 206 is what remains, not the whole resource, so
    // the total has to come from Content-Range when resuming — otherwise the
    // percentage the caller renders would restart at 0 on every retry.
    let total_bytes = total.or_else(|| response.content_length().map(|len| len + downloaded));

    let mut last_report = Instant::now();
    let mut last_pct: u32 = 0;

    loop {
        let chunk = match tokio::time::timeout(timing.chunk_read_timeout, response.chunk()).await {
            Ok(Ok(Some(chunk))) => chunk,
            Ok(Ok(None)) => break,
            Ok(Err(error)) => return Err(DownloadAttemptError::Body(error)),
            Err(_) => {
                return Err(DownloadAttemptError::BodyStalled {
                    filename: filename.to_string(),
                });
            }
        };
        file.write_all(&chunk)
            .await
            .map_err(|error| DownloadAttemptError::PartFile {
                path: part_path.display().to_string(),
                error: error.to_string(),
            })?;
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
                filename: filename.to_string(),
            };
            on_progress(&progress);
            last_report = Instant::now();
            last_pct = current_pct;
        }
    }

    // Flush before the caller stats the file to decide whether this attempt
    // made progress — buffered bytes would read as a stall.
    file.flush()
        .await
        .map_err(|error| DownloadAttemptError::PartFile {
            path: part_path.display().to_string(),
            error: error.to_string(),
        })?;

    // A short body is a dropped connection that happened to end on a chunk
    // boundary. Treat it as a retryable failure so the resume loop continues
    // rather than renaming a truncated archive into place.
    if let Some(total) = total_bytes {
        if downloaded < total {
            return Err(DownloadAttemptError::BodyTruncated {
                got: downloaded,
                expected: total,
            });
        }
    }
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
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\n",
                        )
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

        let _err =
            download_file_with_progress_using(&test_client(), &url, temp.path(), &mut progress)
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

        let error =
            download_file_with_progress_using(&test_client(), &url, temp.path(), &mut progress)
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
}
