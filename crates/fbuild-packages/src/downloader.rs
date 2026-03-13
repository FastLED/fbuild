//! Async HTTP file downloader with SHA256 checksum verification.
//!
//! Uses reqwest async client for parallel downloads.

use std::path::{Path, PathBuf};

use fbuild_core::{FbuildError, Result};
use sha2::{Digest, Sha256};

/// Download a file from a URL into the destination directory (async).
///
/// Returns the path to the downloaded file.
pub async fn download_file(url: &str, dest_dir: &Path) -> Result<PathBuf> {
    let filename = url.rsplit('/').next().unwrap_or("download").to_string();
    let dest_path = dest_dir.join(&filename);

    let response = reqwest::get(url)
        .await
        .map_err(|e| FbuildError::PackageError(format!("failed to download {}: {}", url, e)))?;

    if !response.status().is_success() {
        return Err(FbuildError::PackageError(format!(
            "download failed for {}: HTTP {}",
            url,
            response.status()
        )));
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|e| FbuildError::PackageError(format!("failed to read response body: {}", e)))?;

    tokio::fs::write(&dest_path, &bytes).await.map_err(|e| {
        FbuildError::PackageError(format!(
            "failed to write downloaded file to {}: {}",
            dest_path.display(),
            e
        ))
    })?;

    tracing::debug!("downloaded {} ({} bytes)", filename, bytes.len());
    Ok(dest_path)
}

/// Download multiple files in parallel (async).
///
/// Returns paths to all downloaded files. Fails fast on first error.
pub async fn download_all(urls: &[(&str, &Path)]) -> Result<Vec<PathBuf>> {
    let mut handles = Vec::new();

    for &(url, dest_dir) in urls {
        let url = url.to_string();
        let dest_dir = dest_dir.to_path_buf();
        handles.push(tokio::spawn(
            async move { download_file(&url, &dest_dir).await },
        ));
    }

    let mut results = Vec::new();
    for handle in handles {
        let path = handle
            .await
            .map_err(|e| FbuildError::PackageError(format!("download task failed: {}", e)))??;
        results.push(path);
    }

    Ok(results)
}

/// Verify a file's SHA256 checksum.
pub fn verify_checksum(path: &Path, expected: &str) -> Result<()> {
    let data = std::fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&data);
    let result = hasher.finalize();
    let actual: String = result.iter().map(|b| format!("{:02x}", b)).collect();

    if actual != expected.to_lowercase() {
        return Err(FbuildError::PackageError(format!(
            "checksum mismatch for {}: expected {}, got {}",
            path.display(),
            expected,
            actual
        )));
    }

    Ok(())
}

/// Async version of verify_checksum (reads file with tokio).
pub async fn verify_checksum_async(path: &Path, expected: &str) -> Result<()> {
    let data = tokio::fs::read(path).await?;
    let mut hasher = Sha256::new();
    hasher.update(&data);
    let result = hasher.finalize();
    let actual: String = result.iter().map(|b| format!("{:02x}", b)).collect();

    if actual != expected.to_lowercase() {
        return Err(FbuildError::PackageError(format!(
            "checksum mismatch for {}: expected {}, got {}",
            path.display(),
            expected,
            actual
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_verify_checksum_valid() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"hello world").unwrap();
        f.flush().unwrap();

        // SHA256 of "hello world"
        let expected = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        verify_checksum(f.path(), expected).unwrap();
    }

    #[test]
    fn test_verify_checksum_invalid() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"hello world").unwrap();
        f.flush().unwrap();

        let result = verify_checksum(
            f.path(),
            "0000000000000000000000000000000000000000000000000000000000000000",
        );
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("checksum mismatch"));
    }
}
