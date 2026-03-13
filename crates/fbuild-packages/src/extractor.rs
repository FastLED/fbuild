//! Archive extraction: tar.gz, tar.bz2, zip.
//!
//! Dispatches based on file extension.

use std::path::Path;

use fbuild_core::{FbuildError, Result};

/// Extract an archive into the given directory.
///
/// Supported formats: .tar.gz, .tgz, .tar.bz2, .zip
pub fn extract(archive_path: &Path, dest_dir: &Path) -> Result<()> {
    let name = archive_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();

    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        extract_tar_gz(archive_path, dest_dir)
    } else if name.ends_with(".tar.bz2") {
        extract_tar_bz2(archive_path, dest_dir)
    } else if name.ends_with(".zip") {
        extract_zip(archive_path, dest_dir)
    } else {
        Err(FbuildError::PackageError(format!(
            "unsupported archive format: {}",
            name
        )))
    }
}

fn extract_tar_gz(archive_path: &Path, dest_dir: &Path) -> Result<()> {
    let file = std::fs::File::open(archive_path)?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    archive.unpack(dest_dir).map_err(|e| {
        FbuildError::PackageError(format!(
            "failed to extract {}: {}",
            archive_path.display(),
            e
        ))
    })?;
    Ok(())
}

fn extract_tar_bz2(archive_path: &Path, dest_dir: &Path) -> Result<()> {
    // Use subprocess for bz2 since we don't have a bz2 crate in workspace deps.
    // Fall back to tar command which is available on all supported platforms.
    let result = fbuild_core::subprocess::run_command(
        &[
            "tar",
            "xjf",
            &archive_path.to_string_lossy(),
            "-C",
            &dest_dir.to_string_lossy(),
        ],
        None,
        None,
        None,
    )?;

    if !result.success() {
        return Err(FbuildError::PackageError(format!(
            "failed to extract bz2 archive: {}",
            result.stderr
        )));
    }
    Ok(())
}

fn extract_zip(archive_path: &Path, dest_dir: &Path) -> Result<()> {
    let file = std::fs::File::open(archive_path)?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| {
        FbuildError::PackageError(format!(
            "failed to open zip {}: {}",
            archive_path.display(),
            e
        ))
    })?;

    archive.extract(dest_dir).map_err(|e| {
        FbuildError::PackageError(format!(
            "failed to extract zip {}: {}",
            archive_path.display(),
            e
        ))
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unsupported_format() {
        let result = extract(Path::new("file.xyz"), Path::new("/tmp"));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("unsupported archive format"));
    }
}
