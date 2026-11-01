//! Resolves PlatformIO registry packages from fbuild's own registry index
//! (FastLED/fbuild#1829) instead of PlatformIO's registry API.
//!
//! The index is the mirror manifest from #1815: an `Index` whose per-owner
//! shards list every archive mirrored to fbuild's bucket, with its version,
//! sha256 and the PlatformIO `system` values it serves (`provides`), plus
//! `@names-<h>` shards whose component ids `type/name/owner` map each package
//! name to its owners. A package the
//! index does not list cannot be resolved; there is no PlatformIO fallback.

use std::sync::Arc;

use fbuild_core::platformio_package::{
    PackageKind, RegistryFile, RegistryMetadata, RegistrySpec, RegistryVersion, ResolutionError,
    ResolvedPayload, registry_package_path, resolve_registry,
};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::downloader::{
    CACHE_TTL, MirrorManifest, ShardLookup, load_with, names_key, owner_key, published_manifest,
};

/// Archives resolve to their registry download URL; fbuild's mirror flow
/// then serves them bucket-first.
const REGISTRY_DOWNLOAD_PREFIX: &str = "https://dl.registry.platformio.org/download/";

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error(transparent)]
    Resolution(#[from] ResolutionError),
    #[error("registry archive request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("invalid registry resolution JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("payload cache I/O failed: {0}")]
    Storage(#[from] std::io::Error),
    #[error("invalid payload URL: {0}")]
    InvalidPayloadUrl(String),
    #[error("fbuild's registry index is unavailable: {0}")]
    IndexUnavailable(String),
    #[error(
        "{0} is not in fbuild's registry index; only packages mirrored to fbuild's bucket resolve"
    )]
    NotIndexed(String),
}

/// Where the registry index comes from.
enum IndexSource {
    /// fbuild's published index (`FBUILD_MIRROR_MANIFEST_URL` overrides it).
    Published,
    /// An explicit Index URL, uncached; tests serve one locally.
    At(String),
}

impl IndexSource {
    fn describe(&self) -> &str {
        match self {
            Self::Published => "published",
            Self::At(url) => url,
        }
    }
}

/// Resolves aliases to published, host-specific payload metadata from
/// fbuild's registry index.
pub struct RegistryClient {
    index: IndexSource,
    client: reqwest::Client,
    manifest: tokio::sync::OnceCell<Option<Arc<MirrorManifest>>>,
}

impl Default for RegistryClient {
    fn default() -> Self {
        Self::with_source(IndexSource::Published, fbuild_core::http::client().clone())
    }
}

impl RegistryClient {
    /// A client reading the registry index served at `index_url`.
    #[must_use]
    pub fn new(index_url: impl Into<String>, client: reqwest::Client) -> Self {
        Self::with_source(IndexSource::At(index_url.into()), client)
    }

    fn with_source(index: IndexSource, client: reqwest::Client) -> Self {
        Self {
            index,
            client,
            manifest: tokio::sync::OnceCell::new(),
        }
    }

    pub async fn resolve(
        &self,
        spec: &RegistrySpec,
        kind: PackageKind,
        system: &str,
    ) -> Result<ResolvedPayload, RegistryError> {
        let owner = match &spec.owner {
            Some(owner) => owner.clone(),
            None => self.find_owner(&spec.name, kind).await?,
        };
        let qualified = RegistrySpec {
            owner: Some(owner.clone()),
            ..spec.clone()
        };
        registry_package_path(kind, &qualified)?;
        let metadata = self.package_metadata(&owner, kind, &spec.name).await?;
        Ok(resolve_registry(&qualified, kind, system, &metadata)?)
    }

    /// Every indexed version of `owner`'s `name` package.
    pub async fn package_metadata(
        &self,
        owner: &str,
        kind: PackageKind,
        name: &str,
    ) -> Result<RegistryMetadata, RegistryError> {
        let package = format!("{owner}/{}/{name}", kind.registry_type());
        let shard = match self.index().await?.owner_shard(&owner_key(owner)).await {
            ShardLookup::Loaded(shard) => shard,
            ShardLookup::NotListed => return Err(RegistryError::NotIndexed(package)),
            ShardLookup::Unavailable => {
                return Err(RegistryError::IndexUnavailable(format!(
                    "the shard for owner {owner}"
                )));
            }
        };
        metadata_from_shard(&shard, owner, kind.registry_type(), name)?
            .ok_or(RegistryError::NotIndexed(package))
    }

    /// Owners publishing a `kind` package called `name` (case-insensitive).
    pub async fn owners(
        &self,
        kind: PackageKind,
        name: &str,
    ) -> Result<Vec<String>, RegistryError> {
        let index = self.index().await?;
        if !index.lists_names() {
            return Err(RegistryError::IndexUnavailable(
                "the served index predates the registry index format (no @names shards)".into(),
            ));
        }
        let names = match index.owner_shard(&names_key(name)).await {
            ShardLookup::Loaded(names) => names,
            // No indexed package hashes into this names shard.
            ShardLookup::NotListed => return Ok(Vec::new()),
            ShardLookup::Unavailable => {
                return Err(RegistryError::IndexUnavailable(format!(
                    "the names shard for {name}"
                )));
            }
        };
        let mut owners: Vec<String> = Vec::new();
        for id in names.entries().iter().filter_map(|e| e.id.as_deref()) {
            let Some((package_type, rest)) = id.split_once('/') else {
                continue;
            };
            let Some((package, owner)) = rest.rsplit_once('/') else {
                continue;
            };
            if package_type == kind.registry_type()
                && package.eq_ignore_ascii_case(name)
                && !owners.iter().any(|o| o.eq_ignore_ascii_case(owner))
            {
                owners.push(owner.to_string());
            }
        }
        Ok(owners)
    }

    async fn index(&self) -> Result<Arc<MirrorManifest>, RegistryError> {
        self.manifest
            .get_or_init(|| async {
                match &self.index {
                    IndexSource::Published => published_manifest().await,
                    IndexSource::At(url) => {
                        load_with(&self.client, std::slice::from_ref(url), None, CACHE_TTL)
                            .await
                            .map(Arc::new)
                    }
                }
            })
            .await
            .clone()
            .ok_or_else(|| RegistryError::IndexUnavailable(self.index.describe().to_string()))
    }

    /// Resolve through a host-specific on-disk metadata lock. Check/dry-run
    /// callers pass `fetch = false` and never touch the network. Exact pins
    /// remain immutable; range selections refresh after one hour online.
    pub async fn resolve_cached(
        &self,
        spec: &RegistrySpec,
        kind: PackageKind,
        system: &str,
        cache_root: &Path,
        fetch: bool,
    ) -> Result<Option<ResolvedPayload>, RegistryError> {
        // Boxed `dyn Future`: compiled once instead of inlined per caller.
        self.resolve_cached_boxed(spec, kind, system, cache_root, fetch)
            .await
    }

    #[inline(never)]
    fn resolve_cached_boxed<'a>(
        &'a self,
        spec: &'a RegistrySpec,
        kind: PackageKind,
        system: &'a str,
        cache_root: &'a Path,
        fetch: bool,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Option<ResolvedPayload>, RegistryError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(self.resolve_cached_impl(spec, kind, system, cache_root, fetch))
    }

    async fn resolve_cached_impl(
        &self,
        spec: &RegistrySpec,
        kind: PackageKind,
        system: &str,
        cache_root: &Path,
        fetch: bool,
    ) -> Result<Option<ResolvedPayload>, RegistryError> {
        let request = serde_json::to_vec(&(self.index.describe(), spec, kind, system))?;
        let key = format!("{:x}", Sha256::digest(request));
        let directory = cache_root.join("platformio-registry-resolutions");
        let path = directory.join(format!("{key}.json"));
        let cached = match std::fs::read(&path) {
            Ok(bytes) => {
                let payload: ResolvedPayload = serde_json::from_slice(&bytes)?;
                if payload.name != spec.name
                    || payload.kind != kind
                    || payload.system != system
                    || spec
                        .owner
                        .as_deref()
                        .is_some_and(|owner| owner != payload.owner)
                {
                    return Err(RegistryError::Resolution(ResolutionError::InvalidMetadata(
                        format!("cached registry payload does not match {}", path.display()),
                    )));
                }
                Some(payload)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(RegistryError::Storage(error)),
        };
        if let Some(payload) = &cached {
            let exact_pin = spec
                .requirement
                .as_deref()
                .is_some_and(|requirement| semver::Version::parse(requirement).is_ok());
            let fresh = path
                .metadata()
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| SystemTime::now().duration_since(modified).ok())
                .is_some_and(|age| age < Duration::from_secs(3600));
            if !fetch || exact_pin || fresh {
                return Ok(Some(payload.clone()));
            }
        }
        if !fetch {
            return Ok(None);
        }
        let payload = self.resolve(spec, kind, system).await?;
        std::fs::create_dir_all(&directory)?;
        let mut staged = tempfile::NamedTempFile::new_in(&directory)?;
        staged.write_all(&serde_json::to_vec(&payload)?)?;
        staged
            .persist(&path)
            .map_err(|error| RegistryError::Storage(error.error))?;
        Ok(Some(payload))
    }

    async fn find_owner(&self, name: &str, kind: PackageKind) -> Result<String, RegistryError> {
        let owners = self.owners(kind, name).await?;
        match owners.as_slice() {
            [owner] => Ok(owner.clone()),
            [] => Err(RegistryError::NotIndexed(format!(
                "{}/{name}",
                kind.registry_type()
            ))),
            _ => Err(ResolutionError::AmbiguousAlias {
                name: name.into(),
                owners,
            }
            .into()),
        }
    }
}

/// Versions of `owner/package_type/name` from its owner's shard, whose
/// component ids are `owner/type/name/version/file`. `None` when absent.
fn metadata_from_shard(
    shard: &MirrorManifest,
    owner: &str,
    package_type: &str,
    name: &str,
) -> Result<Option<RegistryMetadata>, RegistryError> {
    let mut metadata: Option<RegistryMetadata> = None;
    for entry in shard.entries() {
        let Some(id) = entry.id.as_deref() else {
            continue;
        };
        let [id_owner, id_type, id_name, version, _file] = id.split('/').collect::<Vec<_>>()[..]
        else {
            continue;
        };
        if id_type != package_type
            || !id_owner.eq_ignore_ascii_case(owner)
            || !id_name.eq_ignore_ascii_case(name)
        {
            continue;
        }
        let download_url = entry
            .urls
            .iter()
            .find(|url| url.starts_with(REGISTRY_DOWNLOAD_PREFIX))
            .ok_or_else(|| {
                ResolutionError::InvalidMetadata(format!("{id} lists no registry download URL"))
            })?;
        let file = RegistryFile {
            systems: if entry.systems.is_empty() {
                vec!["*".to_string()]
            } else {
                entry.systems.clone()
            },
            download_url: download_url.clone(),
            sha256: entry.sha256.clone(),
        };
        let metadata = metadata.get_or_insert_with(|| RegistryMetadata {
            owner: id_owner.to_string(),
            name: id_name.to_string(),
            versions: Vec::new(),
        });
        match metadata.versions.iter_mut().find(|v| v.name == version) {
            Some(existing) => existing.files.push(file),
            None => metadata.versions.push(RegistryVersion {
                name: version.to_string(),
                files: vec![file],
            }),
        }
    }
    Ok(metadata)
}

#[cfg(test)]
#[path = "platformio_registry_tests.rs"]
mod tests;
