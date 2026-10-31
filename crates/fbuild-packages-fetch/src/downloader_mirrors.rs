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

/// Env var listing operator-configured mirrors.
pub(super) const MIRRORS_ENV: &str = "FBUILD_DOWNLOAD_MIRRORS";

/// fbuild's own public mirror bucket (DigitalOcean Spaces `fbuild`, `sfo3`),
/// used when `FBUILD_DOWNLOAD_MIRRORS` is unset (FastLED/fbuild#1806). Its
/// object keys are [`mirror_path`]s.
pub(super) const DEFAULT_MIRROR: &str = "https://fbuild.sfo3.digitaloceanspaces.com/{path}";

/// Mirror bases from a `FBUILD_DOWNLOAD_MIRRORS` value (comma or whitespace
/// separated). Each entry is either a base URL, to which the file name is
/// appended, or a template containing `{filename}` or `{path}`.
fn parse_mirrors(value: &str) -> Vec<String> {
    value
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|m| !m.is_empty())
        .map(str::to_string)
        .collect()
}

/// Candidate URLs for `url` and whether the mirrors lead.
///
/// `FBUILD_DOWNLOAD_MIRRORS` set (even empty, which opts out) means exactly
/// those mirrors, ordered by `FBUILD_DOWNLOAD_MIRROR_FIRST`. Unset means the
/// built-in [`DEFAULT_MIRROR`], tried first unless
/// `FBUILD_DOWNLOAD_MIRROR_FIRST` says otherwise, and only for hosts the
/// bucket mirrors; anything else goes straight to its own URL.
pub(super) fn resolve_candidates(url: &str) -> (Vec<String>, bool) {
    if let Ok(value) = std::env::var(MIRRORS_ENV) {
        return (
            candidate_urls(url, &parse_mirrors(&value)),
            mirror_first_requested(),
        );
    }
    if mirror_path(url).is_none() {
        return (vec![url.to_string()], false);
    }
    let mirror_first = std::env::var(MIRROR_FIRST_ENV).map_or(true, |_| mirror_first_requested());
    (
        candidate_urls(url, &[DEFAULT_MIRROR.to_string()]),
        mirror_first,
    )
}

/// True when `FBUILD_DOWNLOAD_MIRROR_FIRST` is truthy: the mirrors are then
/// tried before the primary URL. Parsed by [`fbuild_core::env_flag`], the one
/// canonical truth table for every fbuild flag (FastLED/fbuild#1798).
pub(super) fn mirror_first_requested() -> bool {
    fbuild_core::env_flag::is_set(MIRROR_FIRST_ENV)
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

/// GitHub, whose archive and release-asset paths a mirror bucket keeps under
/// a `github.com/` prefix so equal file names in different repos never
/// collide (FastLED/fbuild#1806).
const GITHUB_HOST: &str = "github.com";

/// The key a mirror bucket stores `url` under. A PlatformIO registry download
/// maps to everything after `/download/`, e.g.
/// `platformio/tool/framework-arduino-avr/5.4.0/…tar.gz`; a GitHub URL maps
/// to `github.com/<owner>/<repo>/…`. `None` for any other host, including a
/// `/download/` segment off the registry.
fn mirror_path(url: &str) -> Option<String> {
    let (_, after_scheme) = url.split_once("://")?;
    let (host, path) = after_scheme.split_once('/')?;
    match host {
        REGISTRY_DOWNLOAD_HOST => path.strip_prefix("download/").map(str::to_string),
        GITHUB_HOST => Some(format!("{GITHUB_HOST}/{path}")),
        _ => None,
    }
}

/// `url` first, then the same file on each mirror, in order. A mirror entry is
/// a base URL (the file name is appended) or a template substituting
/// `{filename}` or `{path}`; `{path}` is the [`mirror_path`] and falls back
/// to the file name on hosts the bucket does not mirror.
pub(super) fn candidate_urls(url: &str, mirrors: &[String]) -> Vec<String> {
    let filename = url.rsplit('/').next().unwrap_or("download");
    let mirrored = mirror_path(url);
    let path = mirrored.as_deref().unwrap_or(filename);
    let mut urls = vec![url.to_string()];
    for mirror in mirrors {
        urls.push(
            if mirror.contains("{path}") || mirror.contains("{filename}") {
                // Both, so a template naming both never ships a literal
                // placeholder in the request.
                mirror
                    .replace("{path}", path)
                    .replace("{filename}", filename)
            } else {
                format!("{}/{}", mirror.trim_end_matches('/'), filename)
            },
        );
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
    mirror_first: bool,
    timing: RetryTiming,
) -> Result<Vec<u8>> {
    let mut primary_error = None;
    for index in attempt_order(urls.len(), mirror_first) {
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
    mirror_first: bool,
    timing: RetryTiming,
) -> Result<()> {
    // Every candidate lands under the primary URL's file name.
    let filename = urls[0].rsplit('/').next().unwrap_or("download").to_string();
    let mut primary_error = None;
    for index in attempt_order(urls.len(), mirror_first) {
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
