//! ESP32 toolchain metadata resolution — parses tools.json from metadata packages.
//!
//! The metadata flow:
//! 1. platform.json has a metadata URL per toolchain (small ZIP containing tools.json)
//! 2. tools.json has platform-specific (win64/linux-amd64/etc.) download URLs with SHA256
//! 3. Download actual toolchain from the resolved URL

use std::path::{Path, PathBuf};

use fbuild_core::{FbuildError, Result};

/// Resolved toolchain download info from tools.json.
#[derive(Debug)]
pub struct ResolvedToolchain {
    pub url: String,
    pub sha256: Option<String>,
}

/// Detect the current platform key for ESP32 toolchain downloads.
///
/// Returns a key matching the platform entries in tools.json:
/// `"win64"`, `"linux-amd64"`, `"linux-arm64"`, `"macos"`, `"macos-arm64"`
pub fn detect_platform() -> &'static str {
    if cfg!(target_os = "windows") {
        "win64"
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            "macos-arm64"
        } else {
            "macos"
        }
    } else if cfg!(target_arch = "aarch64") {
        "linux-arm64"
    } else {
        "linux-amd64"
    }
}

/// Resolve the platform-specific toolchain URL from a metadata package.
///
/// 1. Downloads the metadata ZIP from `metadata_url`
/// 2. Extracts tools.json
/// 3. Finds the entry for `toolchain_name` and current platform
/// 4. Returns the download URL and SHA256
pub async fn resolve_toolchain_url(
    metadata_url: &str,
    toolchain_name: &str,
    cache_dir: &Path,
) -> Result<ResolvedToolchain> {
    let metadata_dir = cache_dir.join("metadata");

    // Check if tools.json already exists
    if find_tools_json(&metadata_dir).is_none() {
        // Download and extract metadata
        std::fs::create_dir_all(&metadata_dir).map_err(|e| {
            FbuildError::PackageError(format!("failed to create metadata dir: {}", e))
        })?;

        tracing::info!("downloading toolchain metadata");
        let archive_path = crate::downloader::download_file(metadata_url, &metadata_dir).await?;
        crate::extractor::extract(&archive_path, &metadata_dir)?;
        let _ = std::fs::remove_file(&archive_path);
    }

    // Find and parse tools.json
    let tools_json_path = find_tools_json(&metadata_dir).ok_or_else(|| {
        FbuildError::PackageError("tools.json not found in metadata package".into())
    })?;

    parse_tools_json(&tools_json_path, toolchain_name)
}

/// Synchronous wrapper for resolve_toolchain_url.
pub fn resolve_toolchain_url_sync(
    metadata_url: &str,
    toolchain_name: &str,
    cache_dir: &Path,
) -> Result<ResolvedToolchain> {
    let rt = tokio::runtime::Handle::try_current().ok();
    if let Some(handle) = rt {
        handle.block_on(resolve_toolchain_url(
            metadata_url,
            toolchain_name,
            cache_dir,
        ))
    } else {
        let rt = tokio::runtime::Runtime::new().map_err(|e| {
            FbuildError::PackageError(format!("failed to create tokio runtime: {}", e))
        })?;
        rt.block_on(resolve_toolchain_url(
            metadata_url,
            toolchain_name,
            cache_dir,
        ))
    }
}

/// Find tools.json in a directory (may be at root or one level deep).
fn find_tools_json(dir: &Path) -> Option<PathBuf> {
    let direct = dir.join("tools.json");
    if direct.exists() {
        return Some(direct);
    }

    // Check one level deep (archives often nest in a subdirectory)
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let nested = path.join("tools.json");
                if nested.exists() {
                    return Some(nested);
                }
            }
        }
    }

    None
}

/// Parse tools.json to extract the platform-specific toolchain URL.
fn parse_tools_json(path: &Path, toolchain_name: &str) -> Result<ResolvedToolchain> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| FbuildError::PackageError(format!("failed to read tools.json: {}", e)))?;

    let data: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| FbuildError::PackageError(format!("failed to parse tools.json: {}", e)))?;

    let platform = detect_platform();

    let tools = data
        .get("tools")
        .and_then(|t| t.as_array())
        .ok_or_else(|| FbuildError::PackageError("tools.json missing 'tools' array".into()))?;

    for tool in tools {
        let name = tool.get("name").and_then(|n| n.as_str()).unwrap_or("");
        if name != toolchain_name {
            continue;
        }

        let versions = tool
            .get("versions")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                FbuildError::PackageError(format!("no versions for {}", toolchain_name))
            })?;

        if versions.is_empty() {
            return Err(FbuildError::PackageError(format!(
                "empty versions for {}",
                toolchain_name
            )));
        }

        // Use the first version entry
        let version = &versions[0];

        let platform_info = version.get(platform).ok_or_else(|| {
            let available: Vec<&str> = version
                .as_object()
                .map(|m| m.keys().map(|k| k.as_str()).collect())
                .unwrap_or_default();
            FbuildError::PackageError(format!(
                "platform '{}' not found for {}. Available: {:?}",
                platform, toolchain_name, available
            ))
        })?;

        let url = platform_info
            .get("url")
            .and_then(|u| u.as_str())
            .ok_or_else(|| {
                FbuildError::PackageError(format!("no url for {} on {}", toolchain_name, platform))
            })?;

        let sha256 = platform_info
            .get("sha256")
            .and_then(|s| s.as_str())
            .map(|s| s.to_string());

        return Ok(ResolvedToolchain {
            url: url.to_string(),
            sha256,
        });
    }

    Err(FbuildError::PackageError(format!(
        "toolchain '{}' not found in tools.json",
        toolchain_name
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_platform() {
        let p = detect_platform();
        assert!(
            [
                "win64",
                "linux-amd64",
                "linux-arm64",
                "macos",
                "macos-arm64"
            ]
            .contains(&p),
            "unexpected platform: {}",
            p
        );
    }

    #[test]
    fn test_parse_tools_json() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tools_json = tmp.path().join("tools.json");

        let platform = detect_platform();
        let json_content = format!(
            r#"{{
            "tools": [
                {{
                    "name": "toolchain-xtensa-esp-elf",
                    "versions": [
                        {{
                            "{}": {{
                                "url": "https://example.com/xtensa-toolchain.tar.xz",
                                "sha256": "abc123def456"
                            }}
                        }}
                    ]
                }}
            ]
        }}"#,
            platform
        );

        std::fs::write(&tools_json, json_content).unwrap();

        let result = parse_tools_json(&tools_json, "toolchain-xtensa-esp-elf").unwrap();
        assert_eq!(result.url, "https://example.com/xtensa-toolchain.tar.xz");
        assert_eq!(result.sha256, Some("abc123def456".to_string()));
    }

    #[test]
    fn test_parse_tools_json_missing_toolchain() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tools_json = tmp.path().join("tools.json");
        std::fs::write(&tools_json, r#"{"tools": []}"#).unwrap();

        let result = parse_tools_json(&tools_json, "nonexistent");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_tools_json_missing_platform() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tools_json = tmp.path().join("tools.json");
        std::fs::write(
            &tools_json,
            r#"{"tools": [{"name": "tc", "versions": [{"other-platform": {"url": "x"}}]}]}"#,
        )
        .unwrap();

        let result = parse_tools_json(&tools_json, "tc");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not found"));
    }

    #[test]
    fn test_find_tools_json_direct() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tools_json = tmp.path().join("tools.json");
        std::fs::write(&tools_json, "{}").unwrap();
        assert_eq!(find_tools_json(tmp.path()), Some(tools_json));
    }

    #[test]
    fn test_find_tools_json_nested() {
        let tmp = tempfile::TempDir::new().unwrap();
        let subdir = tmp.path().join("metadata-subdir");
        std::fs::create_dir_all(&subdir).unwrap();
        let tools_json = subdir.join("tools.json");
        std::fs::write(&tools_json, "{}").unwrap();
        assert_eq!(find_tools_json(tmp.path()), Some(tools_json));
    }

    #[test]
    fn test_find_tools_json_not_found() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert_eq!(find_tools_json(tmp.path()), None);
    }
}
