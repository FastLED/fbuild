//! Network adapter for the platform-agnostic PlatformIO registry resolver.

use fbuild_core::path::NormalizedPath;
use fbuild_core::platformio_package::{
    PackageKind, RegistrySpec, ResolutionError, ResolvedPayload, registry_api_url,
    resolve_registry_json,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::Path;
use std::time::{Duration, SystemTime};

const API_BASE: &str = "https://api.registry.platformio.org/v3";
const SEARCH_PAGE_SIZE: usize = 50;

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error(transparent)]
    Resolution(#[from] ResolutionError),
    #[error("PlatformIO registry request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("invalid PlatformIO registry search result: {0}")]
    Search(#[from] serde_json::Error),
    #[error("payload cache I/O failed: {0}")]
    Storage(#[from] std::io::Error),
    #[error("invalid payload URL: {0}")]
    InvalidPayloadUrl(String),
    #[error("SHA-256 mismatch for {url}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        url: String,
        expected: String,
        actual: String,
    },
}

/// Resolves aliases to published, host-specific payload metadata. The HTTP
/// client is shared with other package downloads; tests can inject a local
/// registry endpoint without consulting the live service.
pub struct RegistryClient {
    base_url: String,
    client: reqwest::Client,
}

impl Default for RegistryClient {
    fn default() -> Self {
        Self::new(API_BASE, fbuild_core::http::client().clone())
    }
}

impl RegistryClient {
    #[must_use]
    pub fn new(base_url: impl Into<String>, client: reqwest::Client) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            client,
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
        registry_api_url(kind, &qualified)?;
        let url = format!(
            "{}/packages/{}/{}/{}",
            self.base_url,
            owner,
            kind.registry_type(),
            spec.name
        );
        let body = self
            .client
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        Ok(resolve_registry_json(&qualified, kind, system, &body)?)
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
        let request = serde_json::to_vec(&(&self.base_url, spec, kind, system))?;
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

    /// Download a selected registry archive into a digest-keyed cache and
    /// verify it before returning a path. Existing entries are rechecked too.
    pub async fn download_verified(
        &self,
        payload: &ResolvedPayload,
        cache_root: &Path,
    ) -> Result<NormalizedPath, RegistryError> {
        let url = reqwest::Url::parse(&payload.url)
            .map_err(|_| RegistryError::InvalidPayloadUrl(payload.url.clone()))?;
        let filename = url
            .path_segments()
            .and_then(|mut segments| segments.next_back())
            .filter(|name| !name.is_empty() && *name != "." && *name != "..")
            .ok_or_else(|| RegistryError::InvalidPayloadUrl(payload.url.clone()))?;
        let directory = cache_root.join(payload.cache_identity());
        fbuild_core::fs::create_dir_all(&directory).await?;
        let destination = directory.join(filename);
        let mut replace_corrupt = false;
        if destination.is_file() {
            let bytes = fbuild_core::fs::read(&destination).await?;
            if verify_bytes(payload, &bytes).is_ok() {
                return Ok(NormalizedPath::new(destination));
            }
            replace_corrupt = true;
        }
        let bytes = self
            .client
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        verify_bytes(payload, &bytes)?;
        let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
        use std::io::Write;
        temporary.write_all(&bytes)?;
        if replace_corrupt {
            temporary
                .persist(&destination)
                .map_err(|error| RegistryError::Storage(error.error))?;
            return Ok(NormalizedPath::new(destination));
        }
        match temporary.persist_noclobber(&destination) {
            Ok(_) => Ok(NormalizedPath::new(destination)),
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                let cached = fbuild_core::fs::read(&destination).await?;
                verify_bytes(payload, &cached)?;
                Ok(NormalizedPath::new(destination))
            }
            Err(error) => Err(RegistryError::Storage(error.error)),
        }
    }

    async fn find_owner(&self, name: &str, kind: PackageKind) -> Result<String, RegistryError> {
        let mut owners = Vec::new();
        let mut page = 1usize;
        loop {
            let url = format!("{}/search", self.base_url);
            let response = self
                .client
                .get(&url)
                .query(&[
                    ("query", name),
                    ("page", &page.to_string()),
                    ("limit", "50"),
                ])
                .send()
                .await?
                .error_for_status()?
                .text()
                .await?;
            let search: SearchResponse = serde_json::from_str(&response)?;
            for item in &search.items {
                if item.name.eq_ignore_ascii_case(name)
                    && item.package_type == kind.registry_type()
                    && !owners
                        .iter()
                        .any(|owner: &String| owner.eq_ignore_ascii_case(&item.owner.username))
                {
                    owners.push(item.owner.username.clone());
                }
            }
            if page * SEARCH_PAGE_SIZE >= search.total || search.items.is_empty() {
                break;
            }
            page += 1;
        }
        match owners.as_slice() {
            [owner] => Ok(owner.clone()),
            [] => Err(ResolutionError::VersionNotFound {
                package: name.into(),
                requirement: "*".into(),
            }
            .into()),
            _ => Err(ResolutionError::AmbiguousAlias {
                name: name.into(),
                owners,
            }
            .into()),
        }
    }
}

fn verify_bytes(payload: &ResolvedPayload, bytes: &[u8]) -> Result<(), RegistryError> {
    let actual = format!("{:x}", Sha256::digest(bytes));
    if actual == payload.sha256.to_ascii_lowercase() {
        Ok(())
    } else {
        Err(RegistryError::ChecksumMismatch {
            url: payload.url.clone(),
            expected: payload.sha256.clone(),
            actual,
        })
    }
}

#[derive(Deserialize)]
struct SearchResponse {
    total: usize,
    items: Vec<SearchItem>,
}

#[derive(Deserialize)]
struct SearchItem {
    name: String,
    #[serde(rename = "type")]
    package_type: String,
    owner: SearchOwner,
}

#[derive(Deserialize)]
struct SearchOwner {
    username: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, routing::get};
    use fbuild_core::platformio_package::parse_package_spec;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[tokio::test]
    async fn cached_payload_allows_offline_checks_and_warm_resolution() {
        let requests = Arc::new(AtomicUsize::new(0));
        let count = requests.clone();
        let app = Router::new().route(
            "/v3/packages/acme/platform/custom-board",
            get(move || {
                let count = count.clone();
                async move {
                    count.fetch_add(1, Ordering::Relaxed);
                    format!("{{\"name\":\"custom-board\",\"owner\":{{\"username\":\"acme\"}},\"versions\":[{{\"name\":\"1.2.3\",\"files\":[{{\"system\":\"*\",\"download_url\":\"https://example.test/custom.tar.gz\",\"checksum\":{{\"sha256\":\"{HASH}\"}}}}]}}]}}")
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = RegistryClient::new(format!("http://{address}/v3"), reqwest::Client::new());
        let spec = parse_package_spec("acme/custom-board@1.2.3").unwrap();
        let registry = spec.registry().unwrap();
        let cache = tempfile::tempdir().unwrap();
        assert!(
            client
                .resolve_cached(
                    registry,
                    PackageKind::Platform,
                    "linux_x86_64",
                    cache.path(),
                    false
                )
                .await
                .unwrap()
                .is_none()
        );
        let first = client
            .resolve_cached(
                registry,
                PackageKind::Platform,
                "linux_x86_64",
                cache.path(),
                true,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(requests.load(Ordering::Relaxed), 1);
        assert_eq!(
            client
                .resolve_cached(
                    registry,
                    PackageKind::Platform,
                    "linux_x86_64",
                    cache.path(),
                    false
                )
                .await
                .unwrap(),
            Some(first.clone())
        );
        assert_eq!(
            client
                .resolve_cached(
                    registry,
                    PackageKind::Platform,
                    "linux_x86_64",
                    cache.path(),
                    true
                )
                .await
                .unwrap(),
            Some(first)
        );
        assert_eq!(requests.load(Ordering::Relaxed), 1);
        assert!(
            client
                .resolve_cached(
                    registry,
                    PackageKind::Platform,
                    "windows_amd64",
                    cache.path(),
                    false
                )
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn resolves_short_and_qualified_aliases_from_offline_registry() {
        let metadata = format!(
            "{{\"name\":\"custom-board\",\"owner\":{{\"username\":\"acme\"}},\"versions\":[{{\"name\":\"1.2.3\",\"files\":[{{\"system\":\"*\",\"download_url\":\"https://example.test/custom.tar.gz\",\"checksum\":{{\"sha256\":\"{HASH}\"}}}}]}}]}}"
        );
        let app = Router::new()
            .route("/v3/search", get(|| async {
                r#"{"total":1,"items":[{"name":"custom-board","type":"platform","owner":{"username":"acme"}}]}"#
            }))
            .route("/v3/packages/acme/platform/custom-board", get(move || {
                let metadata = metadata.clone();
                async move { metadata }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = RegistryClient::new(format!("http://{address}/v3"), reqwest::Client::new());
        for input in ["custom-board@1.2.3", "acme/custom-board@1.2.3"] {
            let spec = parse_package_spec(input).unwrap();
            let payload = client
                .resolve(
                    spec.registry().unwrap(),
                    PackageKind::Platform,
                    "linux_x86_64",
                )
                .await
                .unwrap();
            assert_eq!(payload.owner, "acme");
            assert_eq!(payload.version, "1.2.3");
            assert_eq!(payload.sha256, HASH);
        }
    }

    #[tokio::test]
    async fn downloaded_payload_is_verified_and_cached_by_identity() {
        let app = Router::new().route(
            "/payload.tar.gz",
            get(|| async { "verified archive bytes" }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = RegistryClient::new(format!("http://{address}/v3"), reqwest::Client::new());
        let payload = ResolvedPayload {
            owner: "acme".into(),
            kind: PackageKind::Platform,
            name: "custom-board".into(),
            version: "1.2.3".into(),
            system: "linux_x86_64".into(),
            url: format!("http://{address}/payload.tar.gz"),
            sha256: format!("{:x}", Sha256::digest(b"verified archive bytes")),
        };
        let cache = tempfile::tempdir().unwrap();
        let path = client
            .download_verified(&payload, cache.path())
            .await
            .unwrap();
        assert_eq!(
            fbuild_core::fs::read(&path).await.unwrap(),
            b"verified archive bytes"
        );
        assert_eq!(
            client
                .download_verified(&payload, cache.path())
                .await
                .unwrap(),
            path
        );
        fbuild_core::fs::write(&path, b"corrupted").await.unwrap();
        assert_eq!(
            client
                .download_verified(&payload, cache.path())
                .await
                .unwrap(),
            path
        );
        assert_eq!(
            fbuild_core::fs::read(&path).await.unwrap(),
            b"verified archive bytes"
        );
        let mut wrong = payload.clone();
        wrong.sha256 = "0".repeat(64);
        assert!(matches!(
            client.download_verified(&wrong, cache.path()).await,
            Err(RegistryError::ChecksumMismatch { .. })
        ));
        assert!(
            !cache
                .path()
                .join(wrong.cache_identity())
                .join("payload.tar.gz")
                .exists()
        );
    }

    #[tokio::test]
    async fn ambiguous_short_alias_does_not_guess_an_owner() {
        let app = Router::new().route(
            "/v3/search",
            get(|| async {
                r#"{"total":2,"items":[
                {"name":"custom-board","type":"platform","owner":{"username":"acme"}},
                {"name":"custom-board","type":"platform","owner":{"username":"other"}}
            ]}"#
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = RegistryClient::new(format!("http://{address}/v3"), reqwest::Client::new());
        let spec = parse_package_spec("custom-board@1.2.3").unwrap();
        assert!(matches!(
            client
                .resolve(
                    spec.registry().unwrap(),
                    PackageKind::Platform,
                    "linux_x86_64"
                )
                .await,
            Err(RegistryError::Resolution(
                ResolutionError::AmbiguousAlias { .. }
            ))
        ));
    }

    #[tokio::test]
    async fn owner_discovery_paginates_at_registry_limit() {
        use axum::extract::Query;
        use std::collections::HashMap;
        let app = Router::new()
            .route("/v3/search", get(|Query(query): Query<HashMap<String, String>>| async move {
                assert_eq!(query.get("limit").map(String::as_str), Some("50"));
                if query.get("page").map(String::as_str) == Some("1") {
                    r#"{"total":51,"items":[{"name":"unrelated","type":"platform","owner":{"username":"other"}}]}"#
                } else {
                    r#"{"total":51,"items":[{"name":"custom-board","type":"platform","owner":{"username":"acme"}}]}"#
                }
            }))
            .route("/v3/packages/acme/platform/custom-board", get(|| async {
                format!("{{\"name\":\"custom-board\",\"owner\":{{\"username\":\"acme\"}},\"versions\":[{{\"name\":\"1.2.3\",\"files\":[{{\"system\":\"*\",\"download_url\":\"https://example.test/custom.tar.gz\",\"checksum\":{{\"sha256\":\"{HASH}\"}}}}]}}]}}")
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = RegistryClient::new(format!("http://{address}/v3"), reqwest::Client::new());
        let spec = parse_package_spec("custom-board@1.2.3").unwrap();
        assert_eq!(
            client
                .resolve(
                    spec.registry().unwrap(),
                    PackageKind::Platform,
                    "linux_x86_64"
                )
                .await
                .unwrap()
                .owner,
            "acme"
        );
    }
}
