//! Mirror fallback for downloads (FastLED/fbuild#1463): the primary URL gets
//! the full retry policy first, then each configured mirror in turn.
//! `FBUILD_DOWNLOAD_MIRROR_FIRST` flips that order for operators who trust
//! their mirror more than the vendor, keeping the primary as the last resort.

use std::path::Path;

use fbuild_core::{FbuildError, Result};

use super::{
    DownloadProgress, RetryTiming, download_file_with_progress_named, get_with_retry_timed,
};

/// Env var that moves the configured mirrors ahead of the primary URL.
pub(super) const MIRROR_FIRST_ENV: &str = "FBUILD_DOWNLOAD_MIRROR_FIRST";

/// Mirror bases from `FBUILD_DOWNLOAD_MIRRORS` (comma or whitespace
/// separated). Each entry is either a base URL, to which the file name is
/// appended, or a template containing `{filename}` or `{path}`.
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

/// True when `FBUILD_DOWNLOAD_MIRROR_FIRST` is truthy: the mirrors are then
/// tried before the primary URL. Empty, `0` and any case-fold of `false` are
/// falsy, matching the convention the rest of fbuild's flags use.
pub(super) fn mirror_first_requested() -> bool {
    let Ok(value) = std::env::var(MIRROR_FIRST_ENV) else {
        return false;
    };
    let value = value.trim();
    !(value.is_empty() || value == "0" || value.eq_ignore_ascii_case("false"))
}

/// Attempt order over [`candidate_urls`]'s output. The primary (index 0)
/// leads; under `mirror_first` the mirrors lead and the primary becomes the
/// last resort, so a stale mirror can never be the only thing tried.
pub(super) fn attempt_order(count: usize, mirror_first: bool) -> Vec<usize> {
    if mirror_first && count > 1 {
        (1..count).chain(std::iter::once(0)).collect()
    } else {
        (0..count).collect()
    }
}

/// Host serving PlatformIO registry downloads, whose `/download/`-relative
/// path a mirror bucket reproduces key for key.
const REGISTRY_DOWNLOAD_HOST: &str = "dl.registry.platformio.org";

/// The registry-relative path of a PlatformIO download URL — everything after
/// `/download/`, e.g. `platformio/tool/framework-arduino-avr/5.4.0/…tar.gz`.
/// `None` for any other URL, including a `/download/` segment on some other
/// host (a GitHub release asset).
fn registry_relative_path(url: &str) -> Option<&str> {
    let (_, after_scheme) = url.split_once("://")?;
    let (host, path) = after_scheme.split_once('/')?;
    if host != REGISTRY_DOWNLOAD_HOST {
        return None;
    }
    path.strip_prefix("download/")
}

/// `url` first, then the same file on each mirror, in order. A mirror entry is
/// a base URL (the file name is appended) or a template substituting
/// `{filename}` or `{path}`; `{path}` keeps the registry's own directory
/// layout and falls back to the file name off the registry.
pub(super) fn candidate_urls(url: &str, mirrors: &[String]) -> Vec<String> {
    let filename = url.rsplit('/').next().unwrap_or("download");
    let path = registry_relative_path(url).unwrap_or(filename);
    let mut urls = vec![url.to_string()];
    for mirror in mirrors {
        urls.push(if mirror.contains("{path}") {
            mirror.replace("{path}", path)
        } else if mirror.contains("{filename}") {
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
    let mut primary_error = None;
    for index in attempt_order(urls.len(), mirror_first_requested()) {
        let url = &urls[index];
        match get_with_retry_timed(client, url, timing).await {
            Ok(bytes) => {
                if index != 0 {
                    tracing::warn!("served from mirror {url}");
                }
                return Ok(bytes);
            }
            Err(error) => {
                tracing::warn!("download from {url} failed: {error}");
                if index == 0 {
                    primary_error = Some(error);
                }
            }
        }
    }
    Err(all_candidates_failed(
        primary_error.expect("the primary URL is always one of the candidates"),
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
    let mut primary_error = None;
    for index in attempt_order(urls.len(), mirror_first_requested()) {
        let url = &urls[index];
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
                if index != 0 {
                    tracing::warn!("served from mirror {url}");
                }
                return Ok(());
            }
            Err(error) => {
                tracing::warn!("download from {url} failed: {error}");
                if index == 0 {
                    primary_error = Some(error);
                }
            }
        }
    }
    Err(all_candidates_failed(
        primary_error.expect("the primary URL is always one of the candidates"),
        urls,
    ))
}
