//! Resolves `lib_deps` libraries from fbuild's registry index
//! (FastLED/fbuild#1829); PlatformIO's registry API is never queried.
//!
//! Supports semver version constraints (e.g. "^1.1.1", ">=3.4.0", "~2.0").

use fbuild_core::platformio_package::{PackageKind, RegistryMetadata};
use fbuild_core::{FbuildError, Result};
use fbuild_packages_fetch::platformio_registry::{RegistryClient, RegistryError};

/// Resolved library version with download URL.
#[derive(Debug, Clone)]
pub struct ResolvedLibrary {
    pub owner: String,
    pub name: String,
    pub version: String,
    pub download_url: String,
}

fn index_error(error: RegistryError) -> FbuildError {
    FbuildError::PackageError(error.to_string())
}

/// Resolve a library with version constraint support. An empty `owner` is
/// looked up by name; see [`select_owner`]. With no constraint the highest
/// indexed version wins.
pub async fn resolve_library(
    owner: &str,
    name: &str,
    version_spec: Option<&str>,
) -> Result<ResolvedLibrary> {
    resolve_library_with(&RegistryClient::default(), owner, name, version_spec).await
}

async fn resolve_library_with(
    client: &RegistryClient,
    owner: &str,
    name: &str,
    version_spec: Option<&str>,
) -> Result<ResolvedLibrary> {
    tracing::info!("resolving library: {}...", name);
    let metadata = if owner.is_empty() {
        select_owner(client, name).await?
    } else {
        client
            .package_metadata(owner, PackageKind::Library, name)
            .await
            .map_err(index_error)?
    };
    let resolved = select_version(&metadata, version_spec)?;
    tracing::info!(
        "resolved {}/{}@{}",
        resolved.owner,
        resolved.name,
        resolved.version
    );
    Ok(resolved)
}

/// The library an owner-less spec names. PlatformIO's search ranked owners
/// by popularity; the index has no such signal, so the owner publishing the
/// most indexed versions wins, and a tie is an error asking for
/// `owner/name`.
async fn select_owner(client: &RegistryClient, name: &str) -> Result<RegistryMetadata> {
    let owners = client
        .owners(PackageKind::Library, name)
        .await
        .map_err(index_error)?;
    let mut candidates = Vec::with_capacity(owners.len());
    for owner in &owners {
        candidates.push(
            client
                .package_metadata(owner, PackageKind::Library, name)
                .await
                .map_err(index_error)?,
        );
    }
    candidates.sort_by_key(|m| std::cmp::Reverse(m.versions.len()));
    match candidates.as_slice() {
        [] => Err(FbuildError::PackageError(format!(
            "library '{name}' is not in fbuild's registry index"
        ))),
        [first, second, ..] if first.versions.len() == second.versions.len() => {
            Err(FbuildError::PackageError(format!(
                "library '{name}' is published by several owners ({}); write it as owner/{name}",
                owners.join(", ")
            )))
        }
        [first, ..] => Ok(first.clone()),
    }
}

/// The highest version of `metadata` satisfying `version_spec`.
fn select_version(
    metadata: &RegistryMetadata,
    version_spec: Option<&str>,
) -> Result<ResolvedLibrary> {
    let version_req = version_spec.and_then(|s| parse_version_req(s.trim()));
    let best = metadata
        .versions
        .iter()
        .filter_map(|version| Some((lenient_parse_version(&version.name)?, version)))
        .filter(|(parsed, _)| version_req.as_ref().is_none_or(|req| req.matches(parsed)))
        .max_by(|(a, _), (b, _)| a.cmp(b));
    let Some((_, version)) = best else {
        return Err(FbuildError::PackageError(format!(
            "no version of {}/{} matches constraint '{}'",
            metadata.owner,
            metadata.name,
            version_spec.unwrap_or("*")
        )));
    };
    let file = version.files.first().ok_or_else(|| {
        FbuildError::PackageError(format!(
            "no download URL for {}/{}@{}",
            metadata.owner, metadata.name, version.name
        ))
    })?;
    Ok(ResolvedLibrary {
        owner: metadata.owner.clone(),
        name: metadata.name.clone(),
        version: version.name.clone(),
        download_url: file.download_url.clone(),
    })
}

/// Leniently parse a version string, padding with `.0` if needed.
///
/// Handles non-standard versions like "2.72" → "2.72.0".
fn lenient_parse_version(s: &str) -> Option<semver::Version> {
    if let Ok(v) = semver::Version::parse(s) {
        return Some(v);
    }
    // Try padding: "2.72" → "2.72.0"
    let padded = format!("{}.0", s);
    semver::Version::parse(&padded).ok()
}

/// Parse a PlatformIO version constraint into a semver::VersionReq.
///
/// Handles formats like: "^1.1.1", ">=3.4.0", "~2.0", "1.5.6", "@^6.3.0"
fn parse_version_req(spec: &str) -> Option<semver::VersionReq> {
    let spec = spec.trim().trim_start_matches('@').trim();
    if spec.is_empty() || spec == "*" {
        return None;
    }
    semver::VersionReq::parse(spec).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    use fbuild_packages_fetch::platformio_registry_fixture::{IndexFile, documents, serve};

    fn lib(owner: &'static str, name: &'static str, version: &'static str) -> IndexFile<'static> {
        IndexFile {
            owner,
            kind: "library",
            name,
            version,
            systems: &[],
            sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        }
    }

    async fn client(files: &[IndexFile<'_>]) -> RegistryClient {
        let (url, _) = serve(documents(files)).await;
        RegistryClient::new(url, reqwest::Client::new())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn owner_and_lenient_versions_resolve_from_the_index() {
        let client = client(&[
            lib("bblanchon", "ArduinoJson", "7.4.2"),
            lib("bblanchon", "ArduinoJson", "7.4.3"),
            lib("bblanchon", "ArduinoJson", "6.21.5"),
            lib("old", "Legacy", "2.72"),
        ])
        .await;
        let latest = resolve_library_with(&client, "bblanchon", "ArduinoJson", Some("^7"))
            .await
            .unwrap();
        assert_eq!(latest.version, "7.4.3");
        assert_eq!(
            latest.download_url,
            "https://dl.registry.platformio.org/download/bblanchon/library/ArduinoJson/7.4.3/ArduinoJson-7.4.3.tar.gz"
        );
        let pinned = resolve_library_with(&client, "", "arduinojson", Some("6.21.5"))
            .await
            .unwrap();
        assert_eq!(
            (pinned.owner.as_str(), pinned.version.as_str()),
            ("bblanchon", "6.21.5")
        );
        let legacy = resolve_library_with(&client, "old", "Legacy", Some("^2.70"))
            .await
            .unwrap();
        assert_eq!(legacy.version, "2.72");
        assert!(
            resolve_library_with(&client, "bblanchon", "ArduinoJson", Some("^9"))
                .await
                .is_err()
        );
        assert!(
            resolve_library_with(&client, "", "Missing", None)
                .await
                .is_err()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn owner_less_specs_pick_the_owner_with_most_versions_or_fail_on_a_tie() {
        let client = client(&[
            lib("fastled", "FastLED", "3.10.0"),
            lib("fastled", "FastLED", "3.10.1"),
            lib("fork", "FastLED", "1.0.0"),
            lib("a", "OneWire", "1.0.0"),
            lib("b", "OneWire", "1.0.0"),
        ])
        .await;
        let fastled = resolve_library_with(&client, "", "FastLED", None)
            .await
            .unwrap();
        assert_eq!(
            (fastled.owner.as_str(), fastled.version.as_str()),
            ("fastled", "3.10.1")
        );
        let tie = resolve_library_with(&client, "", "OneWire", None)
            .await
            .unwrap_err();
        assert!(tie.to_string().contains("owner/OneWire"), "{tie}");
    }

    #[test]
    fn test_parse_version_req_caret() {
        let req = parse_version_req("^1.1.1").unwrap();
        assert!(req.matches(&semver::Version::new(1, 1, 1)));
        assert!(req.matches(&semver::Version::new(1, 9, 0)));
        assert!(!req.matches(&semver::Version::new(2, 0, 0)));
        assert!(!req.matches(&semver::Version::new(0, 9, 0)));
    }

    #[test]
    fn test_parse_version_req_tilde() {
        let req = parse_version_req("~2.0").unwrap();
        assert!(req.matches(&semver::Version::new(2, 0, 0)));
        assert!(req.matches(&semver::Version::new(2, 0, 9)));
        assert!(!req.matches(&semver::Version::new(2, 1, 0)));
    }

    #[test]
    fn test_parse_version_req_exact() {
        let req = parse_version_req("1.5.6").unwrap();
        assert!(req.matches(&semver::Version::new(1, 5, 6)));
    }

    #[test]
    fn test_parse_version_req_with_at() {
        let req = parse_version_req("@ ^6.3.0").unwrap();
        assert!(req.matches(&semver::Version::new(6, 3, 0)));
        assert!(req.matches(&semver::Version::new(6, 12, 0)));
        assert!(!req.matches(&semver::Version::new(7, 0, 0)));
    }

    #[test]
    fn test_parse_version_req_star() {
        assert!(parse_version_req("*").is_none());
    }

    #[test]
    fn test_parse_version_req_empty() {
        assert!(parse_version_req("").is_none());
    }
}
