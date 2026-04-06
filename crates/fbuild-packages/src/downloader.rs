//! Async HTTP file downloader with SHA256 checksum verification.
//!
//! Uses reqwest async client for parallel downloads. Supports streaming
//! downloads with progress reporting for large files.

use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use fbuild_core::{FbuildError, Result};
use sha2::{Digest, Sha256};

fn hex_encode(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{:02x}", b);
            s
        })
}

/// Progress information for a download in progress.
#[derive(Debug, Clone)]
pub struct DownloadProgress {
    pub downloaded: u64,
    pub total_bytes: Option<u64>,
    pub filename: String,
}

impl DownloadProgress {
    /// Format a human-readable progress message.
    pub fn format_message(&self) -> String {
        let dl_mb = self.downloaded as f64 / (1024.0 * 1024.0);
        match self.total_bytes {
            Some(total) => {
                let total_mb = total as f64 / (1024.0 * 1024.0);
                let pct = if total > 0 {
                    (self.downloaded as f64 / total as f64 * 100.0) as u32
                } else {
                    0
                };
                format!(
                    "downloading {}: {:.0}/{:.0} MB ({}%)",
                    self.filename, dl_mb, total_mb, pct
                )
            }
            None => {
                format!("downloading {}: {:.0} MB", self.filename, dl_mb)
            }
        }
    }
}

/// Download a file from a URL into the destination directory (async).
///
/// Returns the path to the downloaded file. Uses buffered download (loads
/// entire response into memory). For large files with progress reporting,
/// use [`download_file_with_progress`].
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

    tracing::info!("downloaded {} ({} bytes)", filename, bytes.len());
    Ok(dest_path)
}

/// Download a file with streaming progress reporting.
///
/// The `on_progress` callback is called periodically during the download
/// (every 15 seconds or every 10% progress, whichever comes first).
pub async fn download_file_with_progress(
    url: &str,
    dest_dir: &Path,
    on_progress: &mut dyn FnMut(&DownloadProgress),
) -> Result<PathBuf> {
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

    let total_bytes = response.content_length();
    let mut downloaded: u64 = 0;
    let mut buf = Vec::with_capacity(total_bytes.unwrap_or(8 * 1024 * 1024) as usize);
    let mut last_report = Instant::now();
    let mut last_pct: u32 = 0;

    let mut stream = response;
    while let Some(chunk) = stream
        .chunk()
        .await
        .map_err(|e| FbuildError::PackageError(format!("failed to read response body: {}", e)))?
    {
        buf.extend_from_slice(&chunk);
        downloaded += chunk.len() as u64;

        let elapsed = last_report.elapsed().as_secs();
        let current_pct = total_bytes
            .map(|t| {
                if t > 0 {
                    (downloaded as f64 / t as f64 * 100.0) as u32
                } else {
                    0
                }
            })
            .unwrap_or(0);
        let pct_jump = current_pct >= last_pct + 10;

        if elapsed >= 15 || pct_jump {
            let progress = DownloadProgress {
                downloaded,
                total_bytes,
                filename: filename.clone(),
            };
            on_progress(&progress);
            last_report = Instant::now();
            last_pct = current_pct;
        }
    }

    tokio::fs::write(&dest_path, &buf).await.map_err(|e| {
        FbuildError::PackageError(format!(
            "failed to write downloaded file to {}: {}",
            dest_path.display(),
            e
        ))
    })?;

    tracing::info!("downloaded {} ({} bytes)", filename, downloaded);
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
    let actual = hex_encode(&result);

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
    let actual = hex_encode(&result);

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

    #[test]
    fn format_download_progress_with_total() {
        let p = DownloadProgress {
            downloaded: 50 * 1024 * 1024,
            total_bytes: Some(150 * 1024 * 1024),
            filename: "toolchain.tar.gz".into(),
        };
        let msg = p.format_message();
        assert!(msg.contains("50"), "msg: {msg}");
        assert!(msg.contains("150"), "msg: {msg}");
        assert!(msg.contains("33%"), "msg: {msg}");
    }

    #[test]
    fn format_download_progress_without_total() {
        let p = DownloadProgress {
            downloaded: 5 * 1024 * 1024,
            total_bytes: None,
            filename: "library.zip".into(),
        };
        let msg = p.format_message();
        assert!(msg.contains("5"), "msg: {msg}");
        assert!(!msg.contains("%"), "msg: {msg}");
    }

    #[test]
    fn format_download_progress_zero() {
        let p = DownloadProgress {
            downloaded: 0,
            total_bytes: Some(100 * 1024 * 1024),
            filename: "file.bin".into(),
        };
        let msg = p.format_message();
        assert!(msg.contains("0%"), "msg: {msg}");
    }
}
