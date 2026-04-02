//! Archive extraction: tar.gz, tar.bz2, tar.xz, tar.zst, zip.
//!
//! All extraction is pure Rust — no subprocess calls.

use std::path::Path;

use fbuild_core::{FbuildError, Result};

/// Extract an archive into the given directory.
///
/// Supported formats: .tar.gz, .tgz, .tar.bz2, .tar.xz, .txz, .tar.zst, .zip
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
    } else if name.ends_with(".tar.xz") || name.ends_with(".txz") {
        extract_tar_xz(archive_path, dest_dir)
    } else if name.ends_with(".tar.zst") {
        extract_tar_zst(archive_path, dest_dir)
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
    let file = std::fs::File::open(archive_path)?;
    let decoder = bzip2::read::BzDecoder::new(file);
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

fn extract_tar_zst(archive_path: &Path, dest_dir: &Path) -> Result<()> {
    let file = std::fs::File::open(archive_path)?;
    let decoder = zstd::Decoder::new(file).map_err(|e| {
        FbuildError::PackageError(format!(
            "failed to open zstd {}: {}",
            archive_path.display(),
            e
        ))
    })?;
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

fn extract_tar_xz(archive_path: &Path, dest_dir: &Path) -> Result<()> {
    let file = std::fs::File::open(archive_path)?;
    let decoder = xz2::read::XzDecoder::new(file);
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

    /// Helper: create a minimal .tar.gz archive containing a single file.
    fn make_tar_gz(dest: &Path, entry_name: &str, content: &[u8]) {
        use flate2::write::GzEncoder;
        use flate2::Compression;

        let file = std::fs::File::create(dest).expect("create tar.gz");
        let enc = GzEncoder::new(file, Compression::default());
        let mut builder = tar::Builder::new(enc);

        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();

        builder
            .append_data(&mut header, entry_name, content)
            .expect("append entry");
        builder
            .into_inner()
            .expect("finish gz")
            .finish()
            .expect("finish encoder");
    }

    /// Helper: create a minimal .tar.xz archive containing a single file.
    fn make_tar_xz(dest: &Path, entry_name: &str, content: &[u8]) {
        use xz2::write::XzEncoder;

        let file = std::fs::File::create(dest).expect("create tar.xz");
        let enc = XzEncoder::new(file, 6);
        let mut builder = tar::Builder::new(enc);

        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();

        builder
            .append_data(&mut header, entry_name, content)
            .expect("append entry");
        builder
            .into_inner()
            .expect("finish xz")
            .finish()
            .expect("finish encoder");
    }

    /// Helper: create a minimal .zip archive containing a single file.
    fn make_zip(dest: &Path, entry_name: &str, content: &[u8]) {
        use std::io::Write;
        use zip::write::SimpleFileOptions;

        let file = std::fs::File::create(dest).expect("create zip");
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file(entry_name, SimpleFileOptions::default())
            .expect("start file");
        zip.write_all(content).expect("write content");
        zip.finish().expect("finish zip");
    }

    #[test]
    fn test_extract_tar_gz() {
        let tmp = tempfile::TempDir::new().unwrap();
        let archive = tmp.path().join("test.tar.gz");
        let dest = tmp.path().join("out");
        std::fs::create_dir_all(&dest).unwrap();

        make_tar_gz(&archive, "hello.txt", b"hello from tar.gz");

        extract(&archive, &dest).expect("extraction should succeed");

        let extracted = dest.join("hello.txt");
        assert!(extracted.exists(), "extracted file should exist");
        let bytes = std::fs::read(&extracted).unwrap();
        assert_eq!(bytes, b"hello from tar.gz");
    }

    #[test]
    fn test_extract_tar_xz() {
        let tmp = tempfile::TempDir::new().unwrap();
        let archive = tmp.path().join("test.tar.xz");
        let dest = tmp.path().join("out");
        std::fs::create_dir_all(&dest).unwrap();

        make_tar_xz(&archive, "hello.txt", b"hello from tar.xz");

        extract(&archive, &dest).expect("extraction should succeed");

        let extracted = dest.join("hello.txt");
        assert!(extracted.exists(), "extracted file should exist");
        let bytes = std::fs::read(&extracted).unwrap();
        assert_eq!(bytes, b"hello from tar.xz");
    }

    #[test]
    fn test_extract_zip() {
        let tmp = tempfile::TempDir::new().unwrap();
        let archive = tmp.path().join("test.zip");
        let dest = tmp.path().join("out");
        std::fs::create_dir_all(&dest).unwrap();

        make_zip(&archive, "hello.txt", b"hello from zip");

        extract(&archive, &dest).expect("extraction should succeed");

        let extracted = dest.join("hello.txt");
        assert!(extracted.exists(), "extracted file should exist");
        let bytes = std::fs::read(&extracted).unwrap();
        assert_eq!(bytes, b"hello from zip");
    }

    /// `extract` dispatches on the *file extension*, not the file magic.
    /// A file with .tar.gz extension that contains invalid data should fail,
    /// not silently extract nothing.
    #[test]
    fn test_invalid_tar_gz_content_errors() {
        let tmp = tempfile::TempDir::new().unwrap();
        let archive = tmp.path().join("bad.tar.gz");
        std::fs::write(&archive, b"this is not a valid gzip stream").unwrap();
        let dest = tmp.path().join("out");
        std::fs::create_dir_all(&dest).unwrap();

        let result = extract(&archive, &dest);
        assert!(result.is_err(), "corrupt tar.gz should return an error");
    }

    /// A .tar.xz with wrong content should fail clearly.
    #[test]
    fn test_invalid_tar_xz_content_errors() {
        let tmp = tempfile::TempDir::new().unwrap();
        let archive = tmp.path().join("bad.tar.xz");
        std::fs::write(&archive, b"this is not a valid xz stream").unwrap();
        let dest = tmp.path().join("out");
        std::fs::create_dir_all(&dest).unwrap();

        let result = extract(&archive, &dest);
        assert!(result.is_err(), "corrupt tar.xz should return an error");
    }

    /// A .zip with wrong content should fail clearly.
    #[test]
    fn test_invalid_zip_content_errors() {
        let tmp = tempfile::TempDir::new().unwrap();
        let archive = tmp.path().join("bad.zip");
        std::fs::write(&archive, b"this is not a valid zip file").unwrap();
        let dest = tmp.path().join("out");
        std::fs::create_dir_all(&dest).unwrap();

        let result = extract(&archive, &dest);
        assert!(result.is_err(), "corrupt zip should return an error");
    }

    /// `.tgz` is an alias for `.tar.gz` and should be handled.
    #[test]
    fn test_extract_tgz_extension() {
        let tmp = tempfile::TempDir::new().unwrap();
        let archive = tmp.path().join("test.tgz");
        let dest = tmp.path().join("out");
        std::fs::create_dir_all(&dest).unwrap();

        make_tar_gz(&archive, "file.txt", b"tgz content");

        extract(&archive, &dest).expect(".tgz should be handled like .tar.gz");
        assert!(dest.join("file.txt").exists());
    }
}
