//! Server-side download fallback settings (FastLED/fbuild#1810, #1815).
//!
//! fbuild publishes [manifest.json](https://github.com/zackees/manifest.json)
//! v1 documents on its GitHub Pages site. A `Release`'s
//! `components[].asset` lists the `urls[]` one artifact can be fetched from,
//! in the order fbuild should try them, plus an optional `sha256`. Changing
//! the documents changes every client's fallback order without an fbuild
//! release.
//!
//! The top document may instead be an `Index` mapping each registry owner to
//! a shard `Release` through a `{url, sha256, size_bytes}` descriptor, so the
//! checksums of every object in the bucket fit without one huge document. A
//! shard is fetched only when a download from its owner is resolved, must
//! hash to its descriptor, and is cached by owner; a stale cached shard is
//! used when no source serves the current one.
//!
//! The document is fetched at most once per process and cached under the
//! cache root. Fallback ladder: fresh cache, then each manifest URL, then the
//! stale cache, then nothing (the caller uses the built-in mirror from
//! FastLED/fbuild#1806). A document that fails validation is never used: one
//! bad entry rejects the whole document, so a half-trusted manifest cannot
//! steer some downloads.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

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
pub(crate) const CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Bodies larger than this are rejected rather than parsed.
pub(super) const MAX_MANIFEST_BYTES: usize = 8 * 1024 * 1024;

/// Per-source fetch budget; the manifest must never stall a build.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// The `kind`s and `schema_version` this reader understands.
const RELEASE_KIND: &str = "Release";
const INDEX_KIND: &str = "Index";
const EXPECTED_SCHEMA_VERSION: u32 = 1;

#[derive(Deserialize)]
struct Document {
    kind: String,
    schema_version: u32,
    #[serde(default)]
    components: Vec<Component>,
    #[serde(default)]
    tools: HashMap<String, ToolDoc>,
}

#[derive(Deserialize)]
struct ToolDoc {
    descriptor: Option<DescriptorDoc>,
}

#[derive(Deserialize)]
struct DescriptorDoc {
    url: String,
    sha256: String,
    size_bytes: u64,
}

/// Where an owner's shard lives and what it must hash to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShardRef {
    /// Absolute URL, or a path relative to the document that listed it.
    pub url: String,
    pub sha256: String,
    pub size_bytes: u64,
}

/// What a loaded manifest needs to fetch its shards later.
#[derive(Debug)]
struct ShardSource {
    client: reqwest::Client,
    /// The manifest sources; a relative shard URL is tried against each.
    bases: Vec<String>,
    cache_dir: Option<NormalizedPath>,
}

#[derive(Deserialize)]
struct Component {
    #[serde(default)]
    id: Option<String>,
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
    #[serde(default)]
    provides: Vec<String>,
}

/// One artifact's fallback settings.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct MirrorEntry {
    /// The component id; in fbuild's shards, `owner/type/name/version/file`.
    pub id: Option<String>,
    /// PlatformIO `system` values the archive serves; empty means any host.
    pub systems: Vec<String>,
    /// Every URL serving this artifact, in the order to try them.
    pub urls: Vec<String>,
    /// Lowercase hex SHA-256 the bytes must match, when published.
    pub sha256: Option<String>,
    /// The publisher withdrew this artifact's mirrors.
    pub yanked: bool,
}

/// A validated manifest, indexed by every URL it lists, plus the shards of
/// an `Index`.
#[derive(Debug, Default)]
pub(crate) struct MirrorManifest {
    by_url: HashMap<String, Arc<MirrorEntry>>,
    /// Every asset entry, in document order.
    entries: Vec<Arc<MirrorEntry>>,
    shards: HashMap<String, ShardRef>,
    source: Option<ShardSource>,
    loaded: Mutex<HashMap<String, Option<Arc<MirrorManifest>>>>,
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
        if doc.kind != RELEASE_KIND && doc.kind != INDEX_KIND {
            return Err(format!(
                "manifest kind is {:?}, expected {RELEASE_KIND:?} or {INDEX_KIND:?}",
                doc.kind
            ));
        }
        if doc.schema_version != EXPECTED_SCHEMA_VERSION {
            return Err(format!(
                "manifest schema_version is {}, expected {EXPECTED_SCHEMA_VERSION}",
                doc.schema_version
            ));
        }
        let shards = if doc.kind == INDEX_KIND {
            validate_shards(doc.tools)?
        } else {
            HashMap::new()
        };
        let mut by_url = HashMap::new();
        let mut entries = Vec::new();
        for (index, component) in doc.components.into_iter().enumerate() {
            let Some(asset) = component.asset else {
                continue;
            };
            let entry = Arc::new(
                validate_asset(component.id, asset)
                    .map_err(|e| format!("component {index}: {e}"))?,
            );
            entries.push(entry.clone());
            for url in &entry.urls {
                if by_url.insert(url.clone(), entry.clone()).is_some() {
                    return Err(format!("component {index}: {url} is listed twice"));
                }
            }
        }
        Ok(Self {
            by_url,
            entries,
            shards,
            ..Self::default()
        })
    }

    /// The entry listing `url` in this document itself, if any.
    #[cfg(test)]
    pub fn lookup(&self, url: &str) -> Option<&MirrorEntry> {
        self.by_url.get(url).map(Arc::as_ref)
    }

    /// The shard descriptor for `owner`, if this is an `Index` listing it.
    #[cfg(test)]
    pub fn shard_ref(&self, owner: &str) -> Option<&ShardRef> {
        self.shards.get(owner)
    }

    /// The entry for `url`: from this document, else from `owner`'s shard,
    /// fetched (once per process) on first use.
    pub async fn entry_for(&self, url: &str, owner: Option<&str>) -> Option<Arc<MirrorEntry>> {
        if let Some(entry) = self.by_url.get(url) {
            return Some(entry.clone());
        }
        let shard = self.shard(owner?).await?;
        shard.by_url.get(url).cloned()
    }

    /// Every asset entry of this document.
    pub(crate) fn entries(&self) -> &[Arc<MirrorEntry>] {
        &self.entries
    }

    /// Whether this Index carries the `@names-` shards of the registry
    /// index format (#1829).
    pub(crate) fn lists_names(&self) -> bool {
        self.shards.keys().any(|key| key.starts_with("@names-"))
    }

    /// `owner`'s shard (an Index key, see [`owner_key`]), telling an owner
    /// the Index does not list apart from a shard that could not be loaded.
    pub(crate) async fn owner_shard(&self, owner: &str) -> ShardLookup {
        if !self.shards.contains_key(owner) {
            return ShardLookup::NotListed;
        }
        self.shard(owner)
            .await
            .map_or(ShardLookup::Unavailable, ShardLookup::Loaded)
    }

    async fn shard(&self, owner: &str) -> Option<Arc<MirrorManifest>> {
        if let Some(hit) = self.loaded_shard(owner) {
            return hit;
        }
        let shard_ref = self.shards.get(owner)?;
        let source = self.source.as_ref()?;
        let shard = load_shard(source, owner, shard_ref).await.map(Arc::new);
        self.loaded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(owner.to_string(), shard.clone());
        shard
    }

    fn loaded_shard(&self, owner: &str) -> Option<Option<Arc<MirrorManifest>>> {
        self.loaded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(owner)
            .cloned()
    }

    fn with_source(mut self, source: ShardSource) -> Self {
        self.source = Some(source);
        self
    }
}

/// Outcome of [`MirrorManifest::owner_shard`].
pub(crate) enum ShardLookup {
    NotListed,
    Unavailable,
    Loaded(Arc<MirrorManifest>),
}

/// Index key of the names shard listing the owners of packages called
/// `name`: `@names-` plus the first hex digit of the SHA-256 of the
/// ASCII-lowercased name, as `names_bucket` in `ci/gen_mirror_manifest.py`.
pub(crate) fn names_key(name: &str) -> String {
    let digest = sha256_hex(name.to_ascii_lowercase().as_bytes());
    format!("@names-{}", &digest[..1])
}

/// The Index key for a registry owner: escaped like `urllib.parse.quote(owner,
/// safe="%")` in `ci/gen_mirror_manifest.py`, so `@` can never be an owner.
pub(crate) fn owner_key(owner: &str) -> String {
    owner
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' | b'%' => {
                char::from(b).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn is_sha256_hex(hex: &str) -> bool {
    hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A shard URL is an allowed absolute URL or a plain relative path that
/// cannot climb out of the directory the manifest was served from.
fn is_allowed_shard_url(url: &str) -> bool {
    if url.contains("://") {
        return is_allowed_url(url);
    }
    !url.is_empty()
        && !url.starts_with('/')
        && !url.contains(['\\', '?', '#', ':'])
        && !url.chars().any(|c| c.is_whitespace() || c.is_control())
        && url
            .split('/')
            .all(|seg| !seg.is_empty() && seg != "." && seg != "..")
}

fn validate_shards(tools: HashMap<String, ToolDoc>) -> Result<HashMap<String, ShardRef>, String> {
    let mut shards = HashMap::new();
    for (owner, tool) in tools {
        let Some(descriptor) = tool.descriptor else {
            return Err(format!("index entry {owner:?} has no descriptor"));
        };
        if owner.is_empty() || owner.contains('/') {
            return Err(format!("index entry {owner:?} is not an owner name"));
        }
        if !is_allowed_shard_url(&descriptor.url) {
            return Err(format!("shard url {:?} is not allowed", descriptor.url));
        }
        if !is_sha256_hex(&descriptor.sha256) {
            return Err(format!(
                "shard {owner:?} sha256 is not 64 lowercase hex digits"
            ));
        }
        if descriptor.size_bytes > MAX_MANIFEST_BYTES as u64 {
            return Err(format!(
                "shard {owner:?} exceeds {MAX_MANIFEST_BYTES} bytes"
            ));
        }
        shards.insert(
            owner,
            ShardRef {
                url: descriptor.url,
                sha256: descriptor.sha256,
                size_bytes: descriptor.size_bytes,
            },
        );
    }
    Ok(shards)
}

/// `rel` resolved against the directory of `base` (a manifest source URL).
fn resolve_shard_url(base: &str, rel: &str) -> String {
    if rel.contains("://") {
        return rel.to_string();
    }
    match base.rfind('/') {
        Some(slash) => format!("{}{rel}", &base[..=slash]),
        None => rel.to_string(),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Parse shard bytes; a shard must be a flat `Release`.
fn parse_shard(bytes: &[u8]) -> Result<MirrorManifest, String> {
    let shard = MirrorManifest::parse(bytes)?;
    if !shard.shards.is_empty() {
        return Err("a shard may not be an Index".to_string());
    }
    Ok(shard)
}

/// Fetch and verify `owner`'s shard: verified cache, each source, then the
/// stale cache.
async fn load_shard(
    source: &ShardSource,
    owner: &str,
    shard_ref: &ShardRef,
) -> Option<MirrorManifest> {
    let cache = source.cache_dir.as_ref().map(|dir| {
        dir.join("shards").join(format!(
            "shard-{}.json",
            &sha256_hex(owner.as_bytes())[..16]
        ))
    });
    let cached = cache.as_ref().and_then(|c| std::fs::read(c.as_path()).ok());
    if let Some(shard) = cached.as_deref().and_then(|b| verified_shard(b, shard_ref)) {
        return Some(shard);
    }
    for base in &source.bases {
        let url = resolve_shard_url(base, &shard_ref.url);
        match fetch_shard(&source.client, &url, shard_ref).await {
            Ok((bytes, shard)) => {
                if let Some(cache) = &cache {
                    write_cache(cache.as_path(), &bytes);
                }
                return Some(shard);
            }
            Err(error) => tracing::warn!("mirror shard {url}: {error}"),
        }
    }
    let shard = cached.and_then(|bytes| parse_shard(&bytes).ok());
    if shard.is_some() {
        tracing::warn!("using a stale cached mirror shard for {owner}");
    }
    shard
}

/// `bytes` parsed as a shard, if they hash to `shard_ref`.
fn verified_shard(bytes: &[u8], shard_ref: &ShardRef) -> Option<MirrorManifest> {
    (sha256_hex(bytes) == shard_ref.sha256)
        .then(|| parse_shard(bytes).ok())
        .flatten()
}

/// Fetch one candidate shard URL and verify it against `shard_ref`.
async fn fetch_shard(
    client: &reqwest::Client,
    url: &str,
    shard_ref: &ShardRef,
) -> Result<(Vec<u8>, MirrorManifest), String> {
    if !is_allowed_url(url) {
        return Err("not an https URL".to_string());
    }
    let limit = usize::try_from(shard_ref.size_bytes).unwrap_or(MAX_MANIFEST_BYTES);
    let bytes = fetch_bytes(client, url, limit).await?;
    if sha256_hex(&bytes) != shard_ref.sha256 {
        return Err("sha256 does not match the index".to_string());
    }
    let shard = parse_shard(&bytes)?;
    Ok((bytes, shard))
}

fn validate_asset(id: Option<String>, asset: AssetDoc) -> Result<MirrorEntry, String> {
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
        hex if is_sha256_hex(hex) => Some(hex.to_string()),
        other => return Err(format!("sha256 {other:?} is not 64 lowercase hex digits")),
    };
    if let Some(bad) = asset
        .provides
        .iter()
        .find(|s| s.is_empty() || s.chars().any(|c| c.is_whitespace() || c.is_control()))
    {
        return Err(format!("provides entry {bad:?} is not a system name"));
    }
    Ok(MirrorEntry {
        id,
        systems: asset.provides,
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
    !host.is_empty()
        && !host.contains('@')
        && !rest.chars().any(|c| c.is_whitespace() || c.is_control())
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
pub(crate) fn manifest_urls() -> Vec<String> {
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
        // `v2`: the registry index format (#1829); an older cached document
        // has no `@names-` shards, so it must not be reused.
        .join(format!("manifest-v2-{key}.json"))
}

/// The process-wide manifest, loaded on first use.
pub(crate) async fn current() -> Option<Arc<MirrorManifest>> {
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

/// Walk the fallback ladder: fresh cache, each source, stale cache. The
/// result can fetch its shards with the same client, sources and cache dir.
pub(crate) async fn load_with(
    client: &reqwest::Client,
    sources: &[String],
    cache: Option<&Path>,
    ttl: Duration,
) -> Option<MirrorManifest> {
    let source = ShardSource {
        client: client.clone(),
        bases: sources.to_vec(),
        cache_dir: cache.and_then(Path::parent).map(NormalizedPath::new),
    };
    load_document(client, sources, cache, ttl)
        .await
        .map(|manifest| manifest.with_source(source))
}

async fn load_document(
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
    let body = fetch_bytes(client, source, MAX_MANIFEST_BYTES).await?;
    let manifest = MirrorManifest::parse(&body)?;
    Ok((body, manifest))
}

/// GET `url`, refusing bodies over `limit` bytes.
async fn fetch_bytes(client: &reqwest::Client, url: &str, limit: usize) -> Result<Vec<u8>, String> {
    let mut response = client.get(url).send().await.map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        body.extend_from_slice(&chunk);
        if body.len() > limit {
            return Err(format!("body exceeds {limit} bytes"));
        }
    }
    Ok(body)
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
