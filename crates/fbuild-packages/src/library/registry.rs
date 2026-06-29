//! PlatformIO registry client for resolving library dependencies.
//!
//! Queries the PlatformIO registry API to find library owners and download URLs.
//! Supports semver version constraints (e.g. "^1.1.1", ">=3.4.0", "~2.0").

use fbuild_core::{FbuildError, Result};

const REGISTRY_API_URL: &str = "https://api.registry.platformio.org/v3";

/// Resolved library version with download URL.
#[derive(Debug, Clone)]
pub struct ResolvedLibrary {
    pub owner: String,
    pub name: String,
    pub version: String,
    pub download_url: String,
}

/// Search the PlatformIO registry for a library by name.
///
/// Returns the owner if found.
pub async fn search_library(name: &str) -> Result<Option<String>> {
    let url = format!("{}/search?query={}", REGISTRY_API_URL, name);
    let response = crate::http::client().get(&url).send().await.map_err(|e| {
        FbuildError::PackageError(format!("registry search failed for {}: {}", name, e))
    })?;

    if !response.status().is_success() {
        return Ok(None);
    }

    let data: serde_json::Value = response.json().await.map_err(|e| {
        FbuildError::PackageError(format!("failed to parse registry response: {}", e))
    })?;

    let items = data.get("items").and_then(|i| i.as_array());
    if let Some(items) = items {
        for item in items {
            let item_name = item.get("name").and_then(|n| n.as_str()).unwrap_or("");
            if item_name.eq_ignore_ascii_case(name) {
                return Ok(item
                    .get("owner")
                    .and_then(|o| o.get("username"))
                    .and_then(|u| u.as_str())
                    .map(|s| s.to_string()));
            }
        }
    }

    Ok(None)
}

/// Resolve a library from the registry with version constraint support.
///
/// Uses the package details API to get all versions, then selects the best
/// match for the given constraint. If no constraint is provided, returns
/// the latest version.
pub async fn resolve_library(
    owner: &str,
    name: &str,
    version_spec: Option<&str>,
) -> Result<ResolvedLibrary> {
    tracing::info!("resolving library: {}...", name);

    // Resolve owner if not specified
    let owner = if owner.is_empty() {
        search_library(name).await?.ok_or_else(|| {
            FbuildError::PackageError(format!("library '{}' not found in registry", name))
        })?
    } else {
        owner.to_string()
    };

    // Use the package details API which returns all versions
    let url = format!("{}/packages/{}/library/{}", REGISTRY_API_URL, owner, name);

    let response = crate::http::client().get(&url).send().await.map_err(|e| {
        FbuildError::PackageError(format!(
            "registry query failed for {}/{}: {}",
            owner, name, e
        ))
    })?;

    if !response.status().is_success() {
        // Fall back to search API if package details fails
        return resolve_library_via_search(&owner, name).await;
    }

    let data: serde_json::Value = response.json().await.map_err(|e| {
        FbuildError::PackageError(format!("failed to parse registry response: {}", e))
    })?;

    // Parse version constraint
    let version_req = version_spec.and_then(|s| parse_version_req(s.trim()));

    // Get all versions from package details
    let versions = data
        .get("versions")
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            FbuildError::PackageError(format!("no versions found for {}/{}", owner, name))
        })?;

    // Find best matching version
    let mut best_match: Option<(semver::Version, &serde_json::Value)> = None;

    for ver_entry in versions {
        let ver_str = match ver_entry.get("name").and_then(|n| n.as_str()) {
            Some(s) => s,
            None => continue,
        };

        let ver = match lenient_parse_version(ver_str) {
            Some(v) => v,
            None => continue,
        };

        // Check if this version satisfies the constraint
        if let Some(ref req) = version_req {
            if !req.matches(&ver) {
                continue;
            }
        }

        // Keep the highest matching version
        if best_match
            .as_ref()
            .map_or(true, |(best_ver, _)| ver > *best_ver)
        {
            best_match = Some((ver, ver_entry));
        }
    }

    let (matched_ver, ver_entry) = best_match.ok_or_else(|| {
        FbuildError::PackageError(format!(
            "no version of {}/{} matches constraint '{}'",
            owner,
            name,
            version_spec.unwrap_or("*")
        ))
    })?;

    // Extract download URL from the matched version
    let download_url = ver_entry
        .get("files")
        .and_then(|f| f.as_array())
        .and_then(|files| files.first())
        .and_then(|file| file.get("download_url"))
        .and_then(|u| u.as_str())
        .ok_or_else(|| {
            FbuildError::PackageError(format!(
                "no download URL for {}/{}@{}",
                owner, name, matched_ver
            ))
        })?;

    let resolved_owner = data
        .get("owner")
        .and_then(|o| o.get("username"))
        .and_then(|u| u.as_str())
        .unwrap_or(&owner);

    let resolved = ResolvedLibrary {
        owner: resolved_owner.to_string(),
        name: data
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or(name)
            .to_string(),
        version: matched_ver.to_string(),
        download_url: download_url.to_string(),
    };

    tracing::info!(
        "resolved {} -> {}/{} v{}",
        name,
        resolved.owner,
        resolved.name,
        resolved.version
    );

    Ok(resolved)
}

/// Fallback: resolve via the search API (no version constraint support).
async fn resolve_library_via_search(owner: &str, name: &str) -> Result<ResolvedLibrary> {
    let url = format!("{}/search?query={}", REGISTRY_API_URL, name);

    let response = crate::http::client().get(&url).send().await.map_err(|e| {
        FbuildError::PackageError(format!(
            "registry search failed for {}/{}: {}",
            owner, name, e
        ))
    })?;

    let data: serde_json::Value = response.json().await.map_err(|e| {
        FbuildError::PackageError(format!("failed to parse registry response: {}", e))
    })?;

    let items = data
        .get("items")
        .and_then(|i| i.as_array())
        .ok_or_else(|| FbuildError::PackageError(format!("no results for {}/{}", owner, name)))?;

    for item in items {
        let item_name = item.get("name").and_then(|n| n.as_str()).unwrap_or("");
        let item_owner = item
            .get("owner")
            .and_then(|o| o.get("username"))
            .and_then(|u| u.as_str())
            .unwrap_or("");

        if item_name.eq_ignore_ascii_case(name)
            && (owner.is_empty() || item_owner.eq_ignore_ascii_case(owner))
        {
            let version_info = item.get("version").ok_or_else(|| {
                FbuildError::PackageError(format!("no version info for {}/{}", owner, name))
            })?;

            let version_str = version_info
                .get("name")
                .and_then(|n| n.as_str())
                .ok_or_else(|| {
                    FbuildError::PackageError(format!("no version name for {}/{}", owner, name))
                })?;

            let download_url = version_info
                .get("files")
                .and_then(|f| f.as_array())
                .and_then(|files| files.first())
                .and_then(|file| file.get("download_url"))
                .and_then(|u| u.as_str())
                .ok_or_else(|| {
                    FbuildError::PackageError(format!(
                        "no download URL for {}/{}@{}",
                        owner, name, version_str
                    ))
                })?;

            return Ok(ResolvedLibrary {
                owner: item_owner.to_string(),
                name: item_name.to_string(),
                version: version_str.to_string(),
                download_url: download_url.to_string(),
            });
        }
    }

    Err(FbuildError::PackageError(format!(
        "library '{}/{}' not found in registry",
        owner, name
    )))
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

    #[test]
    fn test_registry_api_url() {
        assert!(REGISTRY_API_URL.starts_with("https://"));
        assert!(REGISTRY_API_URL.contains("platformio"));
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
