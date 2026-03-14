//! Library download and extraction.
//!
//! Downloads libraries from GitHub URLs or the PlatformIO registry,
//! extracting them into the build directory's libs/ folder.

use std::path::{Path, PathBuf};

use fbuild_core::{FbuildError, Result};

use super::library_spec::LibrarySpec;
use super::registry;

/// Download a library from its spec, returning the library directory.
///
/// - GitHub URL deps: download archive from `{url}/archive/refs/heads/main.zip`
/// - Registry deps: resolve via PlatformIO registry, download `.tar.gz`
///
/// Final layout: `libs_dir/{sanitized_name}/src/` contains library sources.
pub async fn download_library(spec: &LibrarySpec, libs_dir: &Path) -> Result<PathBuf> {
    let lib_name = spec.sanitized_name();
    let lib_dir = libs_dir.join(&lib_name);

    // Check if already downloaded
    let info_file = lib_dir.join("library.json");
    if info_file.exists() && lib_dir.join("src").exists() {
        tracing::debug!("library {} already downloaded", spec.name);
        return Ok(lib_dir);
    }

    std::fs::create_dir_all(&lib_dir).map_err(|e| {
        FbuildError::PackageError(format!(
            "failed to create library dir {}: {}",
            lib_dir.display(),
            e
        ))
    })?;

    if let Some(ref github_url) = spec.github_url {
        download_github_library(github_url, &spec.name, &lib_dir).await?;
    } else {
        download_registry_library(spec, &lib_dir).await?;
    }

    Ok(lib_dir)
}

/// Download a library from a GitHub URL.
async fn download_github_library(url: &str, name: &str, lib_dir: &Path) -> Result<()> {
    // Clean URL: strip .git suffix and trailing slashes
    let clean_url = url.trim_end_matches('/').trim_end_matches(".git");

    // Try main branch first, then master
    let archive_url = format!("{}/archive/refs/heads/main.zip", clean_url);
    tracing::info!("downloading {} from GitHub", name);

    let download_dir = lib_dir.join("_download");
    std::fs::create_dir_all(&download_dir)?;

    let result = crate::downloader::download_file(&archive_url, &download_dir).await;

    let archive_path = match result {
        Ok(path) => path,
        Err(_) => {
            // Try master branch
            let master_url = format!("{}/archive/refs/heads/master.zip", clean_url);
            tracing::debug!("main branch failed, trying master");
            crate::downloader::download_file(&master_url, &download_dir).await?
        }
    };

    // Extract
    let extract_dir = lib_dir.join("_extract");
    std::fs::create_dir_all(&extract_dir)?;
    crate::extractor::extract(&archive_path, &extract_dir)?;

    // Find the actual source directory (GitHub archives have a top-level dir)
    let src_dir = find_extracted_root(&extract_dir);

    // Move to final location
    let final_src = lib_dir.join("src");
    if final_src.exists() {
        std::fs::remove_dir_all(&final_src)?;
    }
    std::fs::rename(&src_dir, &final_src)
        .map_err(|e| FbuildError::PackageError(format!("failed to move library source: {}", e)))?;

    // Clean up
    let _ = std::fs::remove_dir_all(&download_dir);
    let _ = std::fs::remove_dir_all(&extract_dir);

    // Write library.json metadata
    write_library_json(lib_dir, name, "", "github", "")?;

    Ok(())
}

/// Download a library from the PlatformIO registry.
async fn download_registry_library(spec: &LibrarySpec, lib_dir: &Path) -> Result<()> {
    tracing::info!("resolving {} from PlatformIO registry", spec.name);

    let resolved = registry::resolve_library(&spec.owner, &spec.name).await?;

    tracing::info!(
        "downloading {}/{}@{}",
        resolved.owner,
        resolved.name,
        resolved.version
    );

    let download_dir = lib_dir.join("_download");
    std::fs::create_dir_all(&download_dir)?;

    let archive_path =
        crate::downloader::download_file(&resolved.download_url, &download_dir).await?;

    // Extract
    let extract_dir = lib_dir.join("_extract");
    std::fs::create_dir_all(&extract_dir)?;
    crate::extractor::extract(&archive_path, &extract_dir)?;

    // Find the actual source directory
    let src_dir = find_extracted_root(&extract_dir);

    // Move to final location
    let final_src = lib_dir.join("src");
    if final_src.exists() {
        std::fs::remove_dir_all(&final_src)?;
    }
    std::fs::rename(&src_dir, &final_src)
        .map_err(|e| FbuildError::PackageError(format!("failed to move library source: {}", e)))?;

    // Clean up
    let _ = std::fs::remove_dir_all(&download_dir);
    let _ = std::fs::remove_dir_all(&extract_dir);

    // Write library.json metadata
    write_library_json(
        lib_dir,
        &resolved.name,
        &resolved.owner,
        "registry",
        &resolved.version,
    )?;

    Ok(())
}

/// Find the root directory inside an extracted archive.
///
/// Archives often have a single top-level directory (e.g., `FastLED-main/`).
fn find_extracted_root(extract_dir: &Path) -> PathBuf {
    if let Ok(entries) = std::fs::read_dir(extract_dir) {
        let items: Vec<_> = entries.flatten().collect();
        if items.len() == 1 && items[0].path().is_dir() {
            return items[0].path();
        }
    }
    extract_dir.to_path_buf()
}

/// Write library metadata JSON.
fn write_library_json(
    lib_dir: &Path,
    name: &str,
    owner: &str,
    source: &str,
    version: &str,
) -> Result<()> {
    let info = serde_json::json!({
        "name": name,
        "owner": owner,
        "source": source,
        "version": version,
    });

    let path = lib_dir.join("library.json");
    let content = serde_json::to_string_pretty(&info).map_err(|e| {
        FbuildError::PackageError(format!("failed to serialize library info: {}", e))
    })?;

    std::fs::write(&path, content).map_err(|e| {
        FbuildError::PackageError(format!(
            "failed to write library.json to {}: {}",
            path.display(),
            e
        ))
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_extracted_root_single_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let subdir = tmp.path().join("FastLED-main");
        std::fs::create_dir_all(&subdir).unwrap();
        std::fs::write(subdir.join("library.json"), "{}").unwrap();

        assert_eq!(find_extracted_root(tmp.path()), subdir);
    }

    #[test]
    fn test_find_extracted_root_multiple_items() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("dir1")).unwrap();
        std::fs::create_dir_all(tmp.path().join("dir2")).unwrap();

        // Multiple items → return extract dir itself
        assert_eq!(find_extracted_root(tmp.path()), tmp.path().to_path_buf());
    }

    #[test]
    fn test_write_library_json() {
        let tmp = tempfile::TempDir::new().unwrap();
        write_library_json(tmp.path(), "FastLED", "fastled", "registry", "3.7.8").unwrap();

        let content = std::fs::read_to_string(tmp.path().join("library.json")).unwrap();
        assert!(content.contains("FastLED"));
        assert!(content.contains("fastled"));
        assert!(content.contains("3.7.8"));
    }
}
