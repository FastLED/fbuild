//! Server-side download fallback settings (FastLED/fbuild#1810).
//!
//! fbuild publishes a [manifest.json](https://github.com/zackees/manifest.json)
//! v1 `Release` on its GitHub Pages site. Each `components[].asset` lists the
//! `urls[]` one artifact can be fetched from, in the order fbuild should try
//! them, plus an optional `sha256`. Changing that document changes every
//! client's fallback order without an fbuild release.
//!
//! The document is fetched at most once per process and cached under the
//! cache root. Fallback ladder: fresh cache, then each manifest URL, then the
//! stale cache, then nothing (the caller uses the built-in mirror from
//! FastLED/fbuild#1806). A document that fails validation is never used: one
//! bad entry rejects the whole document, so a half-trusted manifest cannot
//! steer some downloads.

use std::collections::HashMap;
use std::path::Path;

use fbuild_core::path::NormalizedPath;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Env var overriding where the manifest is fetched from (comma or
/// whitespace separated). Set but empty disables the manifest.
pub(super) const MANIFEST_URL_ENV: &str = "FBUILD_MIRROR_MANIFEST_URL";

/// Where fbuild's manifest is published: GitHub Pages first, the raw file on
/// `main` as the second source when Pages is down or mid-deploy.
const DEFAULT_MANIFEST_URLS: &[&str] = &[
    "https://fastled.github.io/fbuild/mirror/manifest.json",
    "https://raw.githubusercontent.com/FastLED/fbuild/main/site/mirror/manifest.json",
];

/// A cached copy younger than this is used without asking the server.
pub(super) const CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Bodies larger than this are rejected rather than parsed.
pub(super) const MAX_MANIFEST_BYTES: usize = 8 * 1024 * 1024;

/// Per-source fetch budget; the manifest must never stall a build.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// The only `kind` and `schema_version` this reader understands.
const EXPECTED_KIND: &str = "Release";
const EXPECTED_SCHEMA_VERSION: u32 = 1;

#[derive(Deserialize)]
struct Document {
    kind: String,
    schema_version: u32,
    #[serde(default)]
    components: Vec<Component>,
}

#[derive(Deserialize)]
struct Component {
    asset: Option<AssetDoc>,
}

#[derive(Deserialize)]
struct AssetDoc {
    #[serde(default)]
    urls: Vec<String>,
    #[serde(default)]
    sha256: String,
    #[serde(default)]
    yanked: bool,
}

/// One artifact's fallback settings.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct MirrorEntry {
    /// Every URL serving this artifact, in the order to try them.
    pub urls: Vec<String>,
    /// Lowercase hex SHA-256 the bytes must match, when published.
    pub sha256: Option<String>,
    /// The publisher withdrew this artifact's mirrors.
    pub yanked: bool,
}

/// A validated manifest, indexed by every URL it lists.
#[derive(Debug, Default)]
pub(super) struct MirrorManifest {
    by_url: HashMap<String, Arc<MirrorEntry>>,
}

impl MirrorManifest {
    /// Parse and validate a manifest body. Any invalid entry rejects the
    /// whole document.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(format!(
                "manifest is {} bytes, over the {MAX_MANIFEST_BYTES} byte limit",
                bytes.len()
            ));
        }
        let doc: Document =
            serde_json::from_slice(bytes).map_err(|e| format!("manifest is not valid: {e}"))?;
        if doc.kind != EXPECTED_KIND {
            return Err(format!(
                "manifest kind is {:?}, expected {EXPECTED_KIND:?}",
                doc.kind
            ));
        }
        if doc.schema_version != EXPECTED_SCHEMA_VERSION {
            return Err(format!(
                "manifest schema_version is {}, expected {EXPECTED_SCHEMA_VERSION}",
                doc.schema_version
            ));
        }
        let mut by_url = HashMap::new();
        for (index, component) in doc.components.into_iter().enumerate() {
            let Some(asset) = component.asset else {
                continue;
            };
            let entry =
                Arc::new(validate_asset(asset).map_err(|e| format!("component {index}: {e}"))?);
            for url in &entry.urls {
                if by_url.insert(url.clone(), entry.clone()).is_some() {
                    return Err(format!("component {index}: {url} is listed twice"));
                }
            }
        }
        Ok(Self { by_url })
    }

    /// The entry listing `url`, if any.
    pub fn lookup(&self, url: &str) -> Option<&MirrorEntry> {
        self.by_url.get(url).map(Arc::as_ref)
    }
}

fn validate_asset(asset: AssetDoc) -> Result<MirrorEntry, String> {
    if asset.urls.is_empty() {
        return Err("asset has no urls".to_string());
    }
    for (i, url) in asset.urls.iter().enumerate() {
        if !is_allowed_url(url) {
            return Err(format!("{url:?} is not an https URL"));
        }
        if asset.urls[..i].contains(url) {
            return Err(format!("{url} is listed twice"));
        }
    }
    let sha256 = match asset.sha256.as_str() {
        "" => None,
        hex if hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) => {
            Some(hex.to_string())
        }
        other => return Err(format!("sha256 {other:?} is not 64 lowercase hex digits")),
    };
    Ok(MirrorEntry {
        urls: asset.urls,
        sha256,
        yanked: asset.yanked,
    })
}

/// Production URLs are HTTPS; plain HTTP is allowed only to the loopback
/// host so tests can serve a manifest locally.
pub(super) fn is_allowed_url(url: &str) -> bool {
    let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://").filter(|r| is_loopback(r)))
    else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    !host.is_empty() && !host.contains('@') && !rest.chars().any(char::is_whitespace)
}

/// True when the authority of `rest` (a URL with its scheme removed) is a
/// loopback host.
fn is_loopback(rest: &str) -> bool {
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = if authority.starts_with('[') {
        authority.split(']').next().map(|h| &h[1..]).unwrap_or("")
    } else {
        authority.rsplit_once(':').map_or(authority, |(h, _)| h)
    };
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

/// True when `url` points at the loopback host. Such downloads never consult
/// the manifest.
pub(super) fn is_loopback_url(url: &str) -> bool {
    url.split_once("://")
        .is_some_and(|(_, rest)| is_loopback(rest))
}

/// Manifest sources: `FBUILD_MIRROR_MANIFEST_URL` when set (empty disables),
/// otherwise fbuild's published locations.
pub(super) fn manifest_urls() -> Vec<String> {
    match std::env::var(MANIFEST_URL_ENV) {
        Ok(value) => value
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|u| !u.is_empty())
            .map(str::to_string)
            .collect(),
        Err(_) => DEFAULT_MANIFEST_URLS
            .iter()
            .map(|u| u.to_string())
            .collect(),
    }
}

/// Cache file for a given source list, so an overridden source never
/// shares (or poisons) the default source's cache.
pub(super) fn cache_path(cache_root: &Path, sources: &[String]) -> NormalizedPath {
    let digest = Sha256::digest(sources.join("\n").as_bytes());
    let key: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    NormalizedPath::new(cache_root)
        .join("mirror-manifest")
        .join(format!("manifest-{key}.json"))
}

/// The process-wide manifest, loaded on first use.
pub(super) async fn current() -> Option<Arc<MirrorManifest>> {
    // Unit tests must not reach the network; they drive [`load_with`].
    if cfg!(test) {
        return None;
    }
    static MANIFEST: tokio::sync::OnceCell<Option<Arc<MirrorManifest>>> =
        tokio::sync::OnceCell::const_new();
    MANIFEST
        .get_or_init(|| async {
            let sources = manifest_urls();
            if sources.is_empty() {
                return None;
            }
            let cache = cache_path(&fbuild_paths::get_cache_root(), &sources);
            let client = super::http::client_with_timeout(FETCH_TIMEOUT);
            load_with(&client, &sources, Some(cache.as_path()), CACHE_TTL)
                .await
                .map(Arc::new)
        })
        .await
        .clone()
}

/// Walk the fallback ladder: fresh cache, each source, stale cache.
pub(super) async fn load_with(
    client: &reqwest::Client,
    sources: &[String],
    cache: Option<&Path>,
    ttl: Duration,
) -> Option<MirrorManifest> {
    if let Some(cache) = cache {
        if is_fresh(cache, ttl) {
            if let Some(manifest) = read_cache(cache) {
                return Some(manifest);
            }
        }
    }
    for source in sources {
        match fetch(client, source).await {
            Ok((bytes, manifest)) => {
                if let Some(cache) = cache {
                    write_cache(cache, &bytes);
                }
                return Some(manifest);
            }
            Err(error) => tracing::warn!("mirror manifest {source}: {error}"),
        }
    }
    cache.and_then(read_cache)
}

async fn fetch(
    client: &reqwest::Client,
    source: &str,
) -> Result<(Vec<u8>, MirrorManifest), String> {
    if !is_allowed_url(source) {
        return Err("not an https URL".to_string());
    }
    let mut response = client.get(source).send().await.map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        body.extend_from_slice(&chunk);
        if body.len() > MAX_MANIFEST_BYTES {
            return Err(format!("body exceeds {MAX_MANIFEST_BYTES} bytes"));
        }
    }
    let manifest = MirrorManifest::parse(&body)?;
    Ok((body, manifest))
}

fn is_fresh(path: &Path, ttl: Duration) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age < ttl)
}

fn read_cache(path: &Path) -> Option<MirrorManifest> {
    let bytes = std::fs::read(path).ok()?;
    match MirrorManifest::parse(&bytes) {
        Ok(manifest) => Some(manifest),
        Err(error) => {
            tracing::warn!(
                "ignoring cached mirror manifest {}: {error}",
                path.display()
            );
            None
        }
    }
}

/// Write through a sibling temp file so a concurrent reader never sees a
/// torn document. Failure only costs the cache.
fn write_cache(path: &Path, bytes: &[u8]) {
    let Some(dir) = path.parent() else {
        return;
    };
    let result = std::fs::create_dir_all(dir).and_then(|()| {
        let mut temp = tempfile::NamedTempFile::new_in(dir)?;
        std::io::Write::write_all(&mut temp, bytes)?;
        temp.persist(path).map(drop).map_err(|e| e.error)
    });
    if let Err(error) = result {
        tracing::warn!(
            "could not cache mirror manifest at {}: {error}",
            path.display()
        );
    }
}
