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
    /// The server has nothing left to send (a `416` answer to our range).
    ///
    /// Carried as a flag rather than an empty body because a `416` response
    /// usually *has* a body — S3, GCS and several CDNs send XML or HTML
    /// explaining the error. Streaming that onto the end of an already
    /// complete file would corrupt it, and because those bytes push the
    /// length past the expected total, the short-body check would not catch
    /// it either.
    already_complete: bool,
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
            already_complete: true,
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
            already_complete: false,
        });
    }

    // 200 with a full body. If a range was asked for, the server ignored it.
    let total = response.content_length();
    Ok(OpenedRange {
        response,
        starts_at: 0,
        total,
        already_complete: false,
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
    parse_content_range_value(value)
}

/// Parse the value half of `Content-Range: bytes <start>-<end>/<total>`.
///
/// Split from the header lookup so the grammar — the fragile part — is
/// directly testable. `<total>` may be `*` when the origin does not know the
/// full size, which is legal and yields `None` for the total rather than
/// failing the parse.
fn parse_content_range_value(value: &str) -> Option<(u64, Option<u64>)> {
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

/// Absolute ceiling on attempts, whatever progress is being made.
///
/// The stall budget alone is not a bound: a server that drops the connection
/// after a handful of bytes resets it every single time, and the loop would
/// run forever at a byte rate no operator would accept. There is no outer
/// timeout above this function, so "forever" means a wedged install rather
/// than a failed one — the same non-terminating shape FastLED/fbuild#1370
/// reported, reached from the opposite direction.
///
/// Sized so a genuinely converging download still finishes: the reporter's
/// 282 MB archive over a link dying at ~90 MB needs four attempts, and this
/// leaves an order of magnitude of headroom.
const MAX_TOTAL_ATTEMPTS: u32 = 40;

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

                let exhausted = if stalled >= MAX_STALLED_ATTEMPTS {
                    Some(format!("{stalled} consecutive attempts made no progress"))
                } else if attempt >= MAX_TOTAL_ATTEMPTS {
                    Some(format!(
                        "hit the {MAX_TOTAL_ATTEMPTS}-attempt ceiling while making only intermittent progress"
                    ))
                } else {
                    None
                };

                if error.is_retryable() && exhausted.is_none() {
                    wait_before_retry(url, stalled.max(1), &error, timing).await;
                    continue;
                }

                let _ = tokio::fs::remove_file(&part_path).await;
                let reason = exhausted.unwrap_or_else(|| "error is not retryable".to_string());
                return Err(FbuildError::PackageError(format!(
                    "{} (gave up after {} attempts at byte offset {}; {})",
                    error.into_fbuild_error(url),
                    attempt,
                    resume_from,
                    reason
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
        already_complete,
    } = opened;

    // Nothing left to fetch. Return before touching the file: the `416`
    // response body is an error document, not resource bytes.
    if already_complete {
        return Ok(());
    }

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

    // Set when the body stops arriving. Recorded rather than returned, so the
    // flush below still runs — see the comment there.
    let mut body_error: Option<DownloadAttemptError> = None;

    loop {
        let chunk = match tokio::time::timeout(timing.chunk_read_timeout, response.chunk()).await {
            Ok(Ok(Some(chunk))) => chunk,
            Ok(Ok(None)) => break,
            Ok(Err(error)) => {
                body_error = Some(DownloadAttemptError::Body(error));
                break;
            }
            Err(_) => {
                body_error = Some(DownloadAttemptError::BodyStalled {
                    filename: filename.to_string(),
                });
                break;
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
    //
    // FastLED/fbuild#1370: this has to run on the *failure* path too, which
    // is the whole point. A dropped connection used to return from inside the
    // loop above, skipping this; `tokio::fs::File` makes no promise to flush
    // on drop, so the bytes that did arrive could vanish. The retry loop then
    // stats a short `.part`, resumes from a stale offset, and asks the server
    // for a range it already has — losing exactly the progress this feature
    // exists to keep, on the one path where keeping it matters.
    file.flush()
        .await
        .map_err(|error| DownloadAttemptError::PartFile {
            path: part_path.display().to_string(),
            error: error.to_string(),
        })?;

    if let Some(error) = body_error {
        return Err(error);
    }

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
#[path = "downloader_tests.rs"]
mod tests;
