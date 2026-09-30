//! Mirror fallback for downloads (FastLED/fbuild#1463): the primary URL gets
//! the full retry policy first, then each configured mirror in turn.

use std::path::Path;

use fbuild_core::{FbuildError, Result};

use super::{
    DownloadProgress, RetryTiming, download_file_with_progress_named, get_with_retry_timed,
};

/// Mirror bases from `FBUILD_DOWNLOAD_MIRRORS` (comma or whitespace
/// separated). Each entry is either a base URL, to which the file name is
/// appended, or a template containing `{filename}`.
pub(super) fn configured_mirrors() -> Vec<String> {
    std::env::var("FBUILD_DOWNLOAD_MIRRORS")
        .map(|v| {
            v.split(|c: char| c == ',' || c.is_whitespace())
                .filter(|m| !m.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// `url` first, then the same file on each mirror, in order.
pub(super) fn candidate_urls(url: &str, mirrors: &[String]) -> Vec<String> {
    let filename = url.rsplit('/').next().unwrap_or("download");
    let mut urls = vec![url.to_string()];
    for mirror in mirrors {
        urls.push(if mirror.contains("{filename}") {
            mirror.replace("{filename}", filename)
        } else {
            format!("{}/{}", mirror.trim_end_matches('/'), filename)
        });
    }
    urls
}

/// The error to report when every candidate failed: the primary URL's own
/// failure, plus a note that mirrors were tried.
fn all_candidates_failed(primary: FbuildError, tried: &[String]) -> FbuildError {
    if tried.len() <= 1 {
        return primary;
    }
    FbuildError::PackageError(format!(
        "{primary}; also tried {} mirror(s): {}",
        tried.len() - 1,
        tried[1..].join(", ")
    ))
}

/// Try each candidate URL in turn with the full retry policy; the first
/// success wins. A vendor outage that outlasts the primary's budget falls
/// through to the mirrors (FastLED/fbuild#1463).
pub(super) async fn get_from_candidates(
    client: &reqwest::Client,
    urls: &[String],
    timing: RetryTiming,
) -> Result<Vec<u8>> {
    let mut first_error = None;
    for url in urls {
        match get_with_retry_timed(client, url, timing).await {
            Ok(bytes) => {
                if first_error.is_some() {
                    tracing::warn!("primary download failed; served from mirror {url}");
                }
                return Ok(bytes);
            }
            Err(error) => {
                tracing::warn!("download from {url} failed: {error}");
                first_error.get_or_insert(error);
            }
        }
    }
    Err(all_candidates_failed(
        first_error.expect("at least one candidate URL"),
        urls,
    ))
}

/// Streaming counterpart of [`get_from_candidates`].
pub(super) async fn download_from_candidates(
    client: &reqwest::Client,
    urls: &[String],
    dest_dir: &Path,
    on_progress: &mut (dyn FnMut(&DownloadProgress) + Send),
    timing: RetryTiming,
) -> Result<()> {
    // Every candidate lands under the primary URL's file name.
    let filename = urls[0].rsplit('/').next().unwrap_or("download").to_string();
    let mut first_error = None;
    for url in urls {
        match download_file_with_progress_named(
            client,
            url,
            &filename,
            dest_dir,
            on_progress,
            timing,
        )
        .await
        {
            Ok(()) => {
                if first_error.is_some() {
                    tracing::warn!("primary download failed; served from mirror {url}");
                }
                return Ok(());
            }
            Err(error) => {
                tracing::warn!("download from {url} failed: {error}");
                first_error.get_or_insert(error);
            }
        }
    }
    Err(all_candidates_failed(
        first_error.expect("at least one candidate URL"),
        urls,
    ))
}
