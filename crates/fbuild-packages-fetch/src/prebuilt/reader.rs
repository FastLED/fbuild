//! Catalog-specific reader: deliberately independent of the Release-only mirror reader.
use super::{Asset, MissReason, Query, Resolution, sha256, valid_sha};
use fbuild_core::path::NormalizedPath;
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    path::Path,
    time::{Duration, SystemTime},
};

const TTL: Duration = Duration::from_secs(24 * 60 * 60);
const LIMIT: usize = 8 * 1024 * 1024;
const DEFAULT_SOURCES: &[&str] = &[
    "https://fastled.github.io/fbuild/mirror/manifest.json",
    "https://raw.githubusercontent.com/FastLED/fbuild/main/site/mirror/manifest.json",
];

#[derive(Deserialize)]
struct Index {
    kind: String,
    schema_version: u32,
    tools: BTreeMap<String, Tool>,
}
#[derive(Deserialize)]
struct Tool {
    descriptor: Descriptor,
}
#[derive(Deserialize)]
struct Descriptor {
    url: String,
    sha256: String,
    size_bytes: u64,
}
#[derive(Deserialize)]
struct Catalog {
    kind: String,
    schema_version: u32,
    tool: String,
    channels: BTreeMap<String, String>,
    releases: Vec<Release>,
}
#[derive(Deserialize)]
struct Release {
    tool: String,
    version: String,
    #[serde(default)]
    min_client_version: u32,
    platforms: Vec<ReleasePlatform>,
}
#[derive(Deserialize)]
struct ReleasePlatform {
    platform: Platform,
    variant: Variant,
    asset: Asset,
}
#[derive(Deserialize)]
struct Platform {
    os: String,
    arch: String,
    abi: String,
}
#[derive(Deserialize)]
struct Variant {
    flavor: String,
}

pub struct Reader {
    cache: NormalizedPath,
    sources: Vec<String>,
    client: reqwest::Client,
}
impl Reader {
    pub fn new(root: &Path, sources: Vec<String>, client: reqwest::Client) -> Self {
        Self {
            cache: NormalizedPath::new(root).join("mirror-manifest"),
            sources,
            client,
        }
    }

    pub fn from_environment(root: &Path) -> Self {
        let sources = std::env::var("FBUILD_MIRROR_MANIFEST_URL").map_or_else(
            |_| DEFAULT_SOURCES.iter().map(|s| (*s).to_owned()).collect(),
            |v| {
                v.split(|c: char| c == ',' || c.is_whitespace())
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect()
            },
        );
        Self::new(
            root,
            sources,
            crate::http::client_with_timeout(Duration::from_secs(10)),
        )
    }

    fn index_cache(&self) -> NormalizedPath {
        self.cache.join(format!(
            "prebuilt-index-{}.json",
            &sha256(self.sources.join("\n").as_bytes())[..16]
        ))
    }

    pub(super) fn shard_cache(&self, tool: &str) -> NormalizedPath {
        self.cache.join(format!(
            "prebuilt-shard-{}.json",
            &sha256(format!("{}\n{tool}", self.sources.join("\n")).as_bytes())[..16]
        ))
    }

    pub async fn resolve(&self, query: &Query) -> Resolution {
        self.resolve_with_ttl(query, TTL).await
    }

    pub(super) async fn resolve_with_ttl(&self, query: &Query, ttl: Duration) -> Resolution {
        match self.resolve_inner(query, ttl).await {
            Some(asset) => Resolution::Hit(asset),
            None => Resolution::Miss(MissReason(
                "index unavailable, invalid shard, or no exact fingerprint match".into(),
            )),
        }
    }

    async fn resolve_inner(&self, query: &Query, ttl: Duration) -> Option<Asset> {
        if !valid_sha(&query.fp) || self.sources.is_empty() {
            return None;
        }
        let index_bytes = self
            .load(&self.index_cache(), &self.sources, ttl, |b| {
                parse_index(b).is_some()
            })
            .await?;
        let index = parse_index(&index_bytes)?;
        let tool = format!("prebuilt-{}", query.framework);
        let descriptor = &index.tools.get(&tool)?.descriptor;
        if !valid_sha(&descriptor.sha256) || descriptor.size_bytes > LIMIT as u64 {
            return None;
        }
        let urls: Vec<_> = self
            .sources
            .iter()
            .filter_map(|s| {
                reqwest::Url::parse(s)
                    .ok()?
                    .join(&descriptor.url)
                    .ok()
                    .map(|u| u.to_string())
            })
            .collect();
        let bytes = self
            .load(&self.shard_cache(&tool), &urls, ttl, |b| {
                b.len() as u64 == descriptor.size_bytes
                    && sha256(b) == descriptor.sha256
                    && serde_json::from_slice::<Catalog>(b).is_ok()
            })
            .await?;
        let catalog: Catalog = serde_json::from_slice(&bytes).ok()?;
        if catalog.kind != "Catalog"
            || catalog.schema_version != 1
            || catalog.tool != tool
            || catalog.channels.get(&query.version)? != &query.version
        {
            return None;
        }
        for release in catalog.releases {
            if release.tool != tool
                || release.version != query.version
                || release.min_client_version > 1
            {
                continue;
            }
            for entry in release.platforms {
                if entry.platform.os == "none"
                    && entry.platform.arch == query.arch
                    && entry.platform.abi == query.abi
                    && entry.variant.flavor == format!("fp-{}", &query.fp[..16])
                    && entry
                        .asset
                        .provides
                        .contains(&format!("fbuild-fp:{}", query.fp))
                    && valid_sha(&entry.asset.sha256)
                    && entry.asset.size_bytes > 0
                    && entry.asset.parts.is_empty()
                    && !entry.asset.urls.is_empty()
                    && entry.asset.urls.iter().all(|u| allowed_url(u))
                {
                    let mut asset = entry.asset;
                    let overrides = std::env::var("FBUILD_DOWNLOAD_MIRRORS").ok();
                    let first = std::env::var("FBUILD_DOWNLOAD_MIRROR_FIRST")
                        .map_or(true, |v| fbuild_core::env_flag::parse(&v) == Some(true));
                    asset.urls = super::operator_urls(&asset, query, overrides.as_deref(), first);
                    return (!asset.urls.is_empty()).then_some(asset);
                }
            }
        }
        None
    }

    async fn load(
        &self,
        path: &Path,
        urls: &[String],
        ttl: Duration,
        validate: impl Fn(&[u8]) -> bool,
    ) -> Option<Vec<u8>> {
        let cached = std::fs::read(path)
            .ok()
            .filter(|b| b.len() <= LIMIT && validate(b));
        if cached.is_some() && fresh(path, ttl) {
            return cached;
        }
        for url in urls {
            if let Some(bytes) = self.fetch(url).await.filter(|b| validate(b)) {
                if let Some(parent) = path.parent() {
                    if std::fs::create_dir_all(parent).is_ok() {
                        if let Ok(mut file) = tempfile::NamedTempFile::new_in(parent) {
                            use std::io::Write;
                            if file.write_all(&bytes).is_ok() {
                                let _ = file.persist(path);
                            }
                        }
                    }
                }
                return Some(bytes);
            }
        }
        cached
    }

    async fn fetch(&self, url: &str) -> Option<Vec<u8>> {
        if !allowed_url(url) {
            return None;
        }
        let mut response = self
            .client
            .get(url)
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?;
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.ok()? {
            if bytes.len().checked_add(chunk.len())? > LIMIT {
                return None;
            }
            bytes.extend_from_slice(&chunk);
        }
        Some(bytes)
    }
}

fn parse_index(bytes: &[u8]) -> Option<Index> {
    let index: Index = serde_json::from_slice(bytes).ok()?;
    (index.kind == "Index" && index.schema_version == 1).then_some(index)
}

fn fresh(path: &Path, ttl: Duration) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age < ttl)
}

pub(super) fn allowed_url(url: &str) -> bool {
    reqwest::Url::parse(url).is_ok_and(|u| {
        u.username().is_empty()
            && u.password().is_none()
            && (u.scheme() == "https"
                || (u.scheme() == "http"
                    && matches!(u.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))))
    })
}
