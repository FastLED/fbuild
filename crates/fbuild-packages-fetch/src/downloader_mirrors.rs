//! Mirror fallback for downloads (FastLED/fbuild#1463): the primary URL gets
//! the full retry policy first, then each configured mirror in turn.
//! `FBUILD_DOWNLOAD_MIRROR_FIRST` flips that order for operators who trust
//! their mirror more than the vendor, keeping the primary as the last resort.

use std::path::Path;

use fbuild_core::{FbuildError, Result};
use sha2::Digest;

use super::manifest::{self, MirrorEntry, is_loopback_url};
use super::{
    DownloadProgress, RetryTiming, download_file_with_progress_named, get_with_retry_timed,
    hex_encode,
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

/// Where one download may come from and in which order.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Candidates {
    /// The primary URL at index 0, then every mirror.
    pub urls: Vec<String>,
    /// Indices into `urls` in the order to try them; always includes 0.
    pub order: Vec<usize>,
    /// SHA-256 that bytes served by a mirror must match, when known.
    pub mirror_sha256: Option<String>,
}

impl Candidates {
    /// `urls` tried primary-first, or mirrors-first with the primary last.
    pub fn new(urls: Vec<String>, mirror_first: bool) -> Self {
        let order = attempt_order(urls.len(), mirror_first);
        Self {
            urls,
            order,
            mirror_sha256: None,
        }
    }
}

/// [`resolve_with`] using the server-side manifest (FastLED/fbuild#1810),
/// which is consulted only when no operator mirrors are configured and the
/// download is not to the loopback host.
pub(super) async fn resolve_candidates(url: &str) -> Candidates {
    let manifest = if std::env::var(MIRRORS_ENV).is_err() && !is_loopback_url(url) {
        manifest::current().await
    } else {
        None
    };
    let canonical = canonical_url(url);
    let entry = match &manifest {
        Some(manifest) => {
            manifest
                .entry_for(&canonical, registry_owner(&canonical).as_deref())
                .await
        }
        None => None,
    };
    resolve_with(url, entry.as_deref())
}

/// Candidates for `url`, highest precedence first:
///
/// 1. `FBUILD_DOWNLOAD_MIRRORS` set (even empty, which opts out): exactly
///    those mirrors, ordered by `FBUILD_DOWNLOAD_MIRROR_FIRST`.
/// 2. An entry in the server-side manifest listing `url`: its URLs in the
///    published order (or `FBUILD_DOWNLOAD_MIRROR_FIRST`'s, when set), its
///    SHA-256 guarding mirror bytes. A yanked entry means the primary only.
/// 3. The built-in [`DEFAULT_MIRROR`] for hosts the bucket mirrors, tried
///    first unless `FBUILD_DOWNLOAD_MIRROR_FIRST` says otherwise.
/// 4. The primary URL alone.
pub(super) fn resolve_with(url: &str, entry: Option<&MirrorEntry>) -> Candidates {
    if let Ok(value) = std::env::var(MIRRORS_ENV) {
        return Candidates::new(
            candidate_urls(url, &parse_mirrors(&value)),
            mirror_first_requested(),
        );
    }
    if let Some(entry) = entry {
        return from_manifest(url, entry);
    }
    if mirror_path(url).is_none() {
        return Candidates::new(vec![url.to_string()], false);
    }
    let mirror_first = std::env::var(MIRROR_FIRST_ENV).map_or(true, |_| mirror_first_requested());
    Candidates::new(
        candidate_urls(url, &[DEFAULT_MIRROR.to_string()]),
        mirror_first,
    )
}

/// The form the manifest lists a URL in. The registry API hands out
/// download URLs with a literal space in some owner and package names
/// (`.../download/dmytro korniienko/...`); the manifest, like the request on
/// the wire, has `%20`.
pub(super) fn canonical_url(url: &str) -> String {
    url.replace(' ', "%20")
}

fn from_manifest(url: &str, entry: &MirrorEntry) -> Candidates {
    if entry.yanked {
        return Candidates::new(vec![url.to_string()], false);
    }
    let canonical = canonical_url(url);
    let is_primary = |u: &String| *u == url || *u == canonical;
    let mut urls = vec![url.to_string()];
    urls.extend(entry.urls.iter().filter(|u| !is_primary(u)).cloned());
    let mut order: Vec<usize> = if std::env::var(MIRROR_FIRST_ENV).is_ok() {
        attempt_order(urls.len(), mirror_first_requested())
    } else {
        entry
            .urls
            .iter()
            .filter_map(|u| {
                if is_primary(u) {
                    Some(0)
                } else {
                    urls.iter().position(|c| c == u)
                }
            })
            .collect()
    };
    // The primary is always tried: its error is the one reported, and a
    // manifest that omits it must never leave it out.
    if !order.contains(&0) {
        order.push(0);
    }
    Candidates {
        urls,
        order,
        mirror_sha256: entry.sha256.clone(),
    }
}

/// `Err` naming both digests when `bytes` do not hash to `expected`.
fn check_mirror_digest(bytes: &[u8], expected: &str) -> std::result::Result<(), String> {
    let actual = hex_encode(&sha2::Sha256::digest(bytes));
    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "mirror bytes have sha256 {actual}, manifest expects {expected}"
        ))
    }
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

/// The key a mirror bucket stores `url` under. A PlatformIO registry download
/// maps to everything after `/download/`, e.g.
/// `platformio/tool/framework-arduino-avr/5.4.0/…tar.gz`. `None` for any
/// other host, including GitHub and a `/download/` segment off the registry:
/// the bucket mirrors only the registry layout (FastLED/fbuild#1815).
/// The registry owner of `url` (the first [`mirror_path`] segment), which
/// names its manifest shard.
pub(super) fn registry_owner(url: &str) -> Option<String> {
    mirror_path(url).map(|path| path.split('/').next().unwrap_or_default().to_string())
}

fn mirror_path(url: &str) -> Option<String> {
    let (_, after_scheme) = url.split_once("://")?;
    let (host, path) = after_scheme.split_once('/')?;
    match host {
        REGISTRY_DOWNLOAD_HOST => path.strip_prefix("download/").map(str::to_string),
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

/// True when bytes served by mirror `url` may be used: they match the
/// manifest's SHA-256, or none is known.
fn accept_mirror_bytes(candidates: &Candidates, url: &str, bytes: &[u8]) -> bool {
    if let Some(expected) = &candidates.mirror_sha256 {
        if let Err(error) = check_mirror_digest(bytes, expected) {
            tracing::warn!("rejecting {url}: {error}");
            return false;
        }
    }
    tracing::warn!("served from mirror {url}");
    true
}

/// [`accept_mirror_bytes`] for a file downloaded into `dest_dir`. A rejected
/// file is deleted with its part file, so neither seeds the next
/// candidate's resume.
async fn accept_mirror_file(
    candidates: &Candidates,
    url: &str,
    dest_dir: &Path,
    filename: &str,
) -> bool {
    if candidates.mirror_sha256.is_none() {
        return accept_mirror_bytes(candidates, url, &[]);
    }
    let path = dest_dir.join(filename);
    let accepted = match tokio::fs::read(&path).await {
        Ok(bytes) => accept_mirror_bytes(candidates, url, &bytes),
        Err(error) => {
            tracing::warn!("rejecting {url}: cannot read {}: {error}", path.display());
            false
        }
    };
    if !accepted {
        let _ = tokio::fs::remove_file(&path).await;
        let _ = tokio::fs::remove_file(dest_dir.join(format!("{filename}.part"))).await;
    }
    accepted
}

/// Try each candidate URL in turn with the full retry policy; the first
/// success wins. A vendor outage that outlasts the primary's budget falls
/// through to the mirrors (FastLED/fbuild#1463).
pub(super) async fn get_from_candidates(
    client: &reqwest::Client,
    candidates: &Candidates,
    timing: RetryTiming,
) -> Result<Vec<u8>> {
    let urls = &candidates.urls;
    let mut primary_error = None;
    for &index in &candidates.order {
        let url = &urls[index];
        match get_with_retry_timed(client, url, timing).await {
            Ok(bytes) => {
                if index != 0 && !accept_mirror_bytes(candidates, url, &bytes) {
                    continue;
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
    candidates: &Candidates,
    dest_dir: &Path,
    on_progress: &mut (dyn FnMut(&DownloadProgress) + Send),
    timing: RetryTiming,
) -> Result<()> {
    let urls = &candidates.urls;
    // Every candidate lands under the primary URL's file name.
    let filename = urls[0].rsplit('/').next().unwrap_or("download").to_string();
    let mut primary_error = None;
    for &index in &candidates.order {
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
                if index != 0 && !accept_mirror_file(candidates, url, dest_dir, &filename).await {
                    continue;
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
