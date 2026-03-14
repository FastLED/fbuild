//! PlatformIO registry client for resolving library dependencies.
//!
//! Queries the PlatformIO registry API to find library owners and download URLs.

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
    let response = reqwest::get(&url).await.map_err(|e| {
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
        if let Some(first) = items.first() {
            let owner = first
                .get("owner")
                .and_then(|o| o.get("username"))
                .and_then(|u| u.as_str())
                .map(|s| s.to_string());
            return Ok(owner);
        }
    }

    Ok(None)
}

/// Resolve a library from the registry, returning its download URL.
///
/// If `owner` is empty, searches the registry first to find it.
pub async fn resolve_library(owner: &str, name: &str) -> Result<ResolvedLibrary> {
    // Resolve owner if not specified
    let owner = if owner.is_empty() {
        search_library(name).await?.ok_or_else(|| {
            FbuildError::PackageError(format!("library '{}' not found in registry", name))
        })?
    } else {
        owner.to_string()
    };

    // Search for exact match
    let query = format!("{}/{}", owner, name);
    let url = format!("{}/search?query={}", REGISTRY_API_URL, query);

    let response = reqwest::get(&url).await.map_err(|e| {
        FbuildError::PackageError(format!("registry query failed for {}: {}", query, e))
    })?;

    if !response.status().is_success() {
        return Err(FbuildError::PackageError(format!(
            "registry API error: HTTP {}",
            response.status()
        )));
    }

    let data: serde_json::Value = response.json().await.map_err(|e| {
        FbuildError::PackageError(format!("failed to parse registry response: {}", e))
    })?;

    let items = data
        .get("items")
        .and_then(|i| i.as_array())
        .ok_or_else(|| FbuildError::PackageError(format!("no results for {}/{}", owner, name)))?;

    // Find exact match
    for item in items {
        let item_name = item.get("name").and_then(|n| n.as_str()).unwrap_or("");
        let item_owner = item
            .get("owner")
            .and_then(|o| o.get("username"))
            .and_then(|u| u.as_str())
            .unwrap_or("");

        if item_name.eq_ignore_ascii_case(name)
            && (owner.is_empty() || item_owner.eq_ignore_ascii_case(&owner))
        {
            // Extract version info
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_api_url() {
        assert!(REGISTRY_API_URL.starts_with("https://"));
        assert!(REGISTRY_API_URL.contains("platformio"));
    }
}
