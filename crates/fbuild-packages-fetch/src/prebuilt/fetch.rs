//! Fetch ladder for prebuilt assets (#1882).
//!
//! [`Fetcher`] turns a [`Resolution::Hit`](super::Resolution) asset into a
//! verified file on disk:
//!
//! - the declared exact `size_bytes` and SHA-256 are enforced **while the body
//!   streams**, so an over-long body is refused instead of filling the disk and
//!   a short body is never renamed into place;
//! - a partial download resumes with `Range` + `If-Range` against the
//!   validator (ETag, else Last-Modified) recorded by the attempt that wrote
//!   it, and restarts from byte 0 when the server answers `200` instead of
//!   `206`;
//! - the URL ladder is tried in order, each URL twice: once resuming and once
//!   from scratch, because a poisoned partial can only be cured by starting
//!   over;
//! - concurrent fetches of the same digest in this process collapse to one
//!   download (process-local single-flight; a daemon-wide one is the caller's
//!   job).
//!
//! Nothing here trusts a URL's contents: only the digest decides. Extraction,
//! staging GC and the disk-cache LRU are later cells of #1882.
use super::{Asset, reader::allowed_url, valid_sha};
use fbuild_core::path::NormalizedPath;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Mutex as AsyncMutex,
};

/// Refuse an asset larger than this before allocating anything for it.
pub const MAX_ASSET_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const BLOCK: usize = 64 * 1024;
const CHUNK_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("invalid asset: {0}")]
    Invalid(&'static str),
    #[error("asset {declared} bytes exceeds the {limit} byte limit")]
    TooLarge { declared: u64, limit: u64 },
    #[error("transport failure on {path}: {error}")]
    Transport { path: String, error: String },
    #[error("no source produced a verified asset: {last_error}")]
    Exhausted { last_error: String },
}

/// A verified prebuilt archive on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    pub path: NormalizedPath,
    pub size_bytes: u64,
    pub sha256: String,
}

/// Resume state for a partial body, keyed by the digest it belongs to.
///
/// Written before the body streams so a killed process still leaves a
/// trustworthy `(offset, validator)` pair behind.
#[derive(Serialize, Deserialize)]
struct Partial {
    sha256: String,
    #[serde(default)]
    etag: Option<String>,
    #[serde(default)]
    last_modified: Option<String>,
}

/// The response of one attempt plus how its body must be written.
struct Opened {
    response: reqwest::Response,
    append: bool,
    offset: u64,
}

pub struct Fetcher {
    dest: NormalizedPath,
    client: reqwest::Client,
    limit: u64,
    gates: Mutex<BTreeMap<String, Arc<AsyncMutex<()>>>>,
}

impl Fetcher {
    pub fn new(dest: &Path, client: reqwest::Client) -> Self {
        Self::with_limit(dest, client, MAX_ASSET_BYTES)
    }

    pub fn with_limit(dest: &Path, client: reqwest::Client, limit: u64) -> Self {
        Self {
            dest: NormalizedPath::new(dest),
            client,
            limit,
            gates: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn from_environment(dest: &Path) -> Self {
        Self::new(
            dest,
            crate::http::client_with_timeout(Duration::from_secs(60)),
        )
    }

    /// Download and verify `asset`, or fail without leaving an unverified file
    /// at the returned path.
    pub async fn fetch(&self, asset: &Asset) -> Result<Artifact, FetchError> {
        if !valid_sha(&asset.sha256) {
            return Err(FetchError::Invalid("sha256 must be 64 lowercase hex"));
        }
        if asset.size_bytes == 0 {
            return Err(FetchError::Invalid("size_bytes must be positive"));
        }
        if asset.size_bytes > self.limit {
            return Err(FetchError::TooLarge {
                declared: asset.size_bytes,
                limit: self.limit,
            });
        }
        if !asset.parts.is_empty() {
            return Err(FetchError::Invalid("multi-part assets are unsupported"));
        }
        let final_path = self.dest.join(format!(
            "{}-{}",
            &asset.sha256[..16],
            sanitized(&asset.filename)
        ));
        let gate = self.gate(&asset.sha256);
        let _guard = gate.lock().await;
        if let Some(artifact) = self.verified(&final_path, asset).await {
            return Ok(artifact);
        }
        // Anything at the final path that does not match the digest is debris
        // from an interrupted commit, never a usable hit.
        let _ = tokio::fs::remove_file(final_path.as_path()).await;
        tokio::fs::create_dir_all(self.dest.as_path())
            .await
            .map_err(|error| self.io(&self.dest, error))?;
        let partial = self.dest.join(format!(".{}.partial", asset.sha256));
        let sidecar = self.dest.join(format!(".{}.partial.json", asset.sha256));
        let mut last_error = String::from("asset names no usable https source");
        for url in asset.urls.iter().filter(|url| allowed_url(url)) {
            for fresh in [false, true] {
                if fresh {
                    remove(&partial).await;
                    remove(&sidecar).await;
                }
                let attempt = if fresh {
                    self.attempt(url, asset, &partial, &sidecar, false).await
                } else {
                    self.resume_or_start(url, asset, &partial, &sidecar).await
                };
                if let Err(error) = attempt {
                    last_error = format!("{url}: {error}");
                    continue;
                }
                match self.verified(&partial, asset).await {
                    Some(artifact) => {
                        remove(&final_path).await;
                        tokio::fs::rename(partial.as_path(), final_path.as_path())
                            .await
                            .map_err(|error| self.io(&final_path, error))?;
                        remove(&sidecar).await;
                        return Ok(Artifact {
                            path: final_path,
                            ..artifact
                        });
                    }
                    None => {
                        // A complete body with the wrong digest poisons the
                        // partial; only a clean restart can cure it.
                        remove(&partial).await;
                        remove(&sidecar).await;
                        last_error = format!("{url}: body did not match {}", asset.sha256);
                    }
                }
            }
        }
        Err(FetchError::Exhausted { last_error })
    }

    /// The map holds one gate per digest; a poisoned map still holds valid
    /// gates, and `ban_poison_panic` forbids cascading the panic.
    fn gate(&self, sha: &str) -> Arc<AsyncMutex<()>> {
        let mut gates = self.gates.lock().unwrap_or_else(|error| error.into_inner());
        gates
            .entry(sha.to_owned())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }

    /// Resume when a trustworthy sidecar exists, otherwise start at zero.
    async fn resume_or_start(
        &self,
        url: &str,
        asset: &Asset,
        partial: &NormalizedPath,
        sidecar: &NormalizedPath,
    ) -> Result<(), FetchError> {
        let state = read_state(sidecar)
            .await
            .filter(|state| state.sha256 == asset.sha256);
        let offset = partial_len(partial).await;
        if state.is_none() || offset == 0 || offset >= asset.size_bytes {
            if offset >= asset.size_bytes {
                remove(partial).await;
            }
            remove(sidecar).await;
            return self.attempt(url, asset, partial, sidecar, false).await;
        }
        self.attempt(url, asset, partial, sidecar, true).await
    }

    /// One HTTP attempt: `open` decides whether the body appends to the
    /// partial, `stream` enforces the digest while it arrives.
    async fn attempt(
        &self,
        url: &str,
        asset: &Asset,
        partial: &NormalizedPath,
        sidecar: &NormalizedPath,
        resume: bool,
    ) -> Result<(), FetchError> {
        let opened = self.open(url, asset, partial, sidecar, resume).await?;
        self.stream(url, asset, partial, sidecar, opened).await
    }

    /// Send the request and decide whether its body appends to the partial.
    /// A `resume` attempt asks for `bytes=<partial len>-` guarded by
    /// `If-Range`; a `200` answer means the range was refused, so the body is
    /// written from byte 0.
    async fn open(
        &self,
        url: &str,
        asset: &Asset,
        partial: &NormalizedPath,
        sidecar: &NormalizedPath,
        resume: bool,
    ) -> Result<Opened, FetchError> {
        let offset = if resume {
            partial_len(partial).await
        } else {
            0
        };
        let state = if resume {
            read_state(sidecar).await
        } else {
            None
        };
        let mut request = self.client.get(url);
        let validator = if offset > 0 {
            request = request.header(reqwest::header::RANGE, format!("bytes={offset}-"));
            state
                .as_ref()
                .and_then(|state| state.etag.clone().or_else(|| state.last_modified.clone()))
        } else {
            None
        };
        if let Some(validator) = validator.as_deref() {
            request = request.header(reqwest::header::IF_RANGE, validator);
        }
        let response = request
            .send()
            .await
            .map_err(|error| FetchError::Transport {
                path: url.to_owned(),
                error: error.to_string(),
            })?;
        let status = response.status();
        let etag = header(&response, reqwest::header::ETAG);
        let last_modified = header(&response, reqwest::header::LAST_MODIFIED);
        let append = match status.as_u16() {
            200 => false,
            // A 206 that starts anywhere but `offset` cannot be appended to.
            206 => {
                let start =
                    content_range_start(&response).ok_or_else(|| FetchError::Transport {
                        path: url.to_owned(),
                        error: "206 without a parseable Content-Range".into(),
                    })?;
                offset > 0 && start == offset
            }
            // The range is unsatisfiable: the partial is not a prefix of this
            // resource, so drop it and let the fresh attempt run.
            416 => {
                remove(partial).await;
                remove(sidecar).await;
                return Err(FetchError::Transport {
                    path: url.to_owned(),
                    error: "416 range not satisfiable; partial discarded".into(),
                });
            }
            other => {
                return Err(FetchError::Transport {
                    path: url.to_owned(),
                    error: format!("unexpected status {other}"),
                });
            }
        };
        self.write_sidecar(sidecar, asset, etag, last_modified)
            .await;
        Ok(Opened {
            response,
            append,
            offset,
        })
    }

    /// Write the body into the partial while hashing it, so an over-long body
    /// is refused before it writes past the declared size and a short one is
    /// never committed.
    async fn stream(
        &self,
        url: &str,
        asset: &Asset,
        partial: &NormalizedPath,
        sidecar: &NormalizedPath,
        opened: Opened,
    ) -> Result<(), FetchError> {
        let Opened {
            mut response,
            append,
            offset,
        } = opened;
        let mut hasher = Sha256::new();
        if append {
            hash_prefix(partial, &mut hasher)
                .await
                .map_err(|error| self.io(partial, error))?;
        }
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(append)
            .truncate(!append)
            .open(partial.as_path())
            .await
            .map_err(|error| self.io(partial, error))?;
        let mut total = if append { offset } else { 0 };
        loop {
            let chunk = match tokio::time::timeout(CHUNK_TIMEOUT, response.chunk()).await {
                Ok(Ok(Some(chunk))) => chunk,
                Ok(Ok(None)) => break,
                Ok(Err(error)) => {
                    return Err(FetchError::Transport {
                        path: url.to_owned(),
                        error: error.to_string(),
                    });
                }
                Err(_) => {
                    return Err(FetchError::Transport {
                        path: url.to_owned(),
                        error: "body stalled".into(),
                    });
                }
            };
            total += chunk.len() as u64;
            if total > asset.size_bytes {
                // Never write past the declared size, and never resume from a
                // prefix of the wrong resource.
                remove(partial).await;
                remove(sidecar).await;
                return Err(FetchError::Transport {
                    path: url.to_owned(),
                    error: format!("body exceeds the declared {} bytes", asset.size_bytes),
                });
            }
            file.write_all(&chunk)
                .await
                .map_err(|error| self.io(partial, error))?;
            hasher.update(&chunk);
        }
        file.flush()
            .await
            .map_err(|error| self.io(partial, error))?;
        if total != asset.size_bytes {
            return Err(FetchError::Transport {
                path: url.to_owned(),
                error: format!("body ended at {total} of {} bytes", asset.size_bytes),
            });
        }
        let actual = format!("{:x}", hasher.finalize());
        if actual != asset.sha256 {
            remove(partial).await;
            remove(sidecar).await;
            return Err(FetchError::Transport {
                path: url.to_owned(),
                error: format!("sha256 mismatch: got {actual}"),
            });
        }
        Ok(())
    }

    async fn write_sidecar(
        &self,
        sidecar: &NormalizedPath,
        asset: &Asset,
        etag: Option<String>,
        last_modified: Option<String>,
    ) {
        let state = Partial {
            sha256: asset.sha256.clone(),
            etag,
            last_modified,
        };
        if let Ok(bytes) = serde_json::to_vec(&state) {
            let _ = tokio::fs::write(sidecar.as_path(), bytes).await;
        }
    }

    async fn verified(&self, path: &NormalizedPath, asset: &Asset) -> Option<Artifact> {
        let metadata = tokio::fs::metadata(path.as_path()).await.ok()?;
        if metadata.len() != asset.size_bytes {
            return None;
        }
        let mut hasher = Sha256::new();
        hash_prefix(path, &mut hasher).await.ok()?;
        (format!("{:x}", hasher.finalize()) == asset.sha256).then(|| Artifact {
            path: path.clone(),
            size_bytes: asset.size_bytes,
            sha256: asset.sha256.clone(),
        })
    }

    fn io(&self, path: &NormalizedPath, error: std::io::Error) -> FetchError {
        FetchError::Transport {
            path: path.display_slash(),
            error: error.to_string(),
        }
    }
}

fn header(response: &reqwest::Response, name: reqwest::header::HeaderName) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

/// Start byte of `Content-Range: bytes <start>-<end>/<total>`.
fn content_range_start(response: &reqwest::Response) -> Option<u64> {
    header(response, reqwest::header::CONTENT_RANGE)?
        .strip_prefix("bytes ")?
        .split('-')
        .next()?
        .trim()
        .parse()
        .ok()
}

async fn read_state(sidecar: &NormalizedPath) -> Option<Partial> {
    tokio::fs::read(sidecar.as_path())
        .await
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

async fn partial_len(path: &NormalizedPath) -> u64 {
    tokio::fs::metadata(path.as_path())
        .await
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

async fn hash_prefix(path: &NormalizedPath, hasher: &mut Sha256) -> std::io::Result<()> {
    let mut file = tokio::fs::File::open(path.as_path()).await?;
    let mut buffer = vec![0u8; BLOCK];
    loop {
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            return Ok(());
        }
        hasher.update(&buffer[..read]);
    }
}

async fn remove(path: &NormalizedPath) {
    let _ = tokio::fs::remove_file(path.as_path()).await;
}

/// Keep the destination name short and path-safe: a bucket filename is input,
/// never a path.
fn sanitized(filename: &str) -> String {
    let base = filename
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("asset.bin")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    let base = base.trim_start_matches('.');
    if base.is_empty() {
        return "asset.bin".into();
    }
    base.chars().take(48).collect()
}

#[cfg(test)]
#[path = "fetch_tests.rs"]
mod tests;
