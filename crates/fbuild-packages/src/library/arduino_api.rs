//! ArduinoCore-API submodule fetcher.
//!
//! Many Arduino framework packages (ArduinoCore-megaavr, ArduinoCore-renesas)
//! depend on the ArduinoCore-API library, which provides `api/ArduinoAPI.h`,
//! `api/Stream.h`, etc. GitHub archive downloads exclude git submodules, so
//! we must fetch and inject ArduinoCore-API separately.
//!
//! This mirrors Arduino's own release process (see ArduinoCore-megaavr's
//! `.github/workflows/release.yaml`) which checks out ArduinoCore-API and
//! copies its `api/` directory into `cores/arduino/`.

use std::path::Path;

use fbuild_core::Result;

/// Version of ArduinoCore-API to use.
const ARDUINO_API_VERSION: &str = "1.5.2";
const ARDUINO_API_URL: &str =
    "https://github.com/arduino/ArduinoCore-API/archive/refs/tags/1.5.2.tar.gz";

/// Ensure the ArduinoCore-API `api/` directory exists in the framework's core dir.
///
/// If `{core_dir}/api/ArduinoAPI.h` already exists, this is a no-op.
/// Otherwise, downloads ArduinoCore-API and copies its `api/` subdirectory
/// into `{core_dir}/api/`.
///
/// # Arguments
/// * `core_dir` - The framework's `cores/arduino/` directory (or equivalent)
pub fn ensure_arduino_api(core_dir: &Path) -> Result<()> {
    let api_marker = core_dir.join("api").join("ArduinoAPI.h");
    if api_marker.exists() {
        tracing::debug!(
            "ArduinoCore-API already present at {}",
            core_dir.join("api").display()
        );
        return Ok(());
    }

    tracing::info!(
        "Fetching ArduinoCore-API v{} for {}",
        ARDUINO_API_VERSION,
        core_dir.display()
    );

    // Download to a temp directory
    let tmp_dir = tempfile::TempDir::new().map_err(|e| {
        fbuild_core::FbuildError::PackageError(format!("failed to create temp dir: {}", e))
    })?;

    // Use blocking reqwest since we may or may not be in an async context
    let response = reqwest::blocking::get(ARDUINO_API_URL).map_err(|e| {
        fbuild_core::FbuildError::PackageError(format!("failed to download ArduinoCore-API: {}", e))
    })?;

    if !response.status().is_success() {
        return Err(fbuild_core::FbuildError::PackageError(format!(
            "failed to download ArduinoCore-API: HTTP {}",
            response.status()
        )));
    }

    let archive_path = tmp_dir.path().join("ArduinoCore-API.tar.gz");
    let bytes = response.bytes().map_err(|e| {
        fbuild_core::FbuildError::PackageError(format!("failed to read response: {}", e))
    })?;
    std::fs::write(&archive_path, &bytes)?;

    // Extract
    let extract_dir = tmp_dir.path().join("extracted");
    std::fs::create_dir_all(&extract_dir)?;
    crate::extractor::extract(&archive_path, &extract_dir)?;

    // Find the api/ directory inside the extracted archive
    // Structure: ArduinoCore-API-1.4.2/api/
    let api_src = find_api_dir(&extract_dir).ok_or_else(|| {
        fbuild_core::FbuildError::PackageError(
            "ArduinoCore-API archive missing api/ directory".to_string(),
        )
    })?;

    // Remove any existing api/ (may be a dangling symlink from the archive).
    // ArduinoCore-renesas has `api` as a symlink to ../../../ArduinoCore-API/api/
    // which is dangling after archive extraction.
    let api_dest = core_dir.join("api");
    let is_symlink = std::fs::symlink_metadata(&api_dest)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false);
    if is_symlink {
        // Remove dangling symlink (works on both Unix and Windows)
        let _ = std::fs::remove_file(&api_dest);
    } else if api_dest.is_dir() {
        let _ = std::fs::remove_dir_all(&api_dest);
    }

    // Copy api/ into the core directory
    copy_dir_recursive(&api_src, &api_dest)?;

    // Verify
    if !api_marker.exists() {
        return Err(fbuild_core::FbuildError::PackageError(format!(
            "ArduinoCore-API installation failed: {} not found after copy",
            api_marker.display()
        )));
    }

    tracing::info!("ArduinoCore-API installed to {}", api_dest.display());
    Ok(())
}

/// Find the `api/` directory inside an extracted ArduinoCore-API archive.
fn find_api_dir(extract_dir: &Path) -> Option<std::path::PathBuf> {
    // Direct: extract_dir/api/
    if extract_dir.join("api").is_dir() {
        return Some(extract_dir.join("api"));
    }

    // One level deep: extract_dir/ArduinoCore-API-x.y.z/api/
    if let Ok(entries) = std::fs::read_dir(extract_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && path.join("api").is_dir() {
                return Some(path.join("api"));
            }
        }
    }

    None
}

/// Recursively copy a directory.
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;

    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_api_dir_direct() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("api")).unwrap();
        std::fs::write(tmp.path().join("api/ArduinoAPI.h"), "").unwrap();
        let found = find_api_dir(tmp.path());
        assert!(found.is_some());
        assert!(found.unwrap().join("ArduinoAPI.h").exists());
    }

    #[test]
    fn test_find_api_dir_nested() {
        let tmp = tempfile::TempDir::new().unwrap();
        let nested = tmp.path().join("ArduinoCore-API-1.4.2");
        std::fs::create_dir_all(nested.join("api")).unwrap();
        std::fs::write(nested.join("api/ArduinoAPI.h"), "").unwrap();
        let found = find_api_dir(tmp.path());
        assert!(found.is_some());
    }

    #[test]
    fn test_find_api_dir_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let found = find_api_dir(tmp.path());
        assert!(found.is_none());
    }

    #[test]
    fn test_copy_dir_recursive() {
        let src = tempfile::TempDir::new().unwrap();
        let dst = tempfile::TempDir::new().unwrap();

        std::fs::create_dir_all(src.path().join("sub")).unwrap();
        std::fs::write(src.path().join("file.h"), "header").unwrap();
        std::fs::write(src.path().join("sub/nested.h"), "nested").unwrap();

        let dst_dir = dst.path().join("output");
        copy_dir_recursive(src.path(), &dst_dir).unwrap();

        assert!(dst_dir.join("file.h").exists());
        assert!(dst_dir.join("sub/nested.h").exists());
    }
}
