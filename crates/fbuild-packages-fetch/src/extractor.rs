//! Archive extraction: tar.gz, tar.bz2, tar.xz, tar.zst, zip.
//!
//! All extraction is pure Rust — no subprocess calls.

use std::path::Path;

pub use crate::extractor_manifest::extract_with_manifest;
use fbuild_core::{FbuildError, Result};

/// Check the actual extracted layout before an installer marks it complete.
fn validate_extraction_tree(root: &Path, required: bool) -> Result<()> {
    const LEGACY_DIRECTORY_UNITS: usize = 248;
    if !required {
        return Ok(());
    }
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        let entry = entry.map_err(|error| {
            FbuildError::PackageError(format!(
                "could not validate extracted Windows paths: {error}"
            ))
        })?;
        if fbuild_core::platform::fs::legacy_path_units(entry.path()) + 1 > LEGACY_DIRECTORY_UNITS {
            return Err(FbuildError::PackageError(format!(
                "Windows short-path fallback cannot safely use extracted path {}. Run fbuild --fix-win or use a shorter package location.",
                entry.path().display()
            )));
        }
    }
    Ok(())
}

pub(crate) fn validate_extraction(root: &Path) -> Result<()> {
    validate_extraction_tree(
        root,
        fbuild_paths::long_paths::current_resolution()
            .is_some_and(|resolution| resolution.observation.requires_fallback()),
    )
}

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

/// Extract a `.zip` archive. Public so callers (e.g. lnk materializer)
/// can dispatch without depending on the source file's extension.
pub fn extract_zip_public(archive_path: &Path, dest_dir: &Path) -> Result<()> {
    extract_zip(archive_path, dest_dir)
}

/// Extract a `.tar.gz` archive. Public so callers (e.g. lnk materializer)
/// can dispatch without depending on the source file's extension.
pub fn extract_tar_gz_public(archive_path: &Path, dest_dir: &Path) -> Result<()> {
    extract_tar_gz(archive_path, dest_dir)
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
    validate_extraction(dest_dir)?;
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
    validate_extraction(dest_dir)?;
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
    validate_extraction(dest_dir)?;
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
    validate_extraction(dest_dir)?;
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
    validate_extraction(dest_dir)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_path_fallback_rejects_sdk_members_that_still_exceed_the_limit() {
        let root = tempfile::tempdir().unwrap();
        let deep = (0..18).fold(root.path().to_path_buf(), |path, _| {
            path.join("sdk-component")
        });
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("header.h"), b"// SDK header").unwrap();
        let error = validate_extraction_tree(root.path(), true)
            .unwrap_err()
            .to_string();
        assert!(error.contains("--fix-win"), "{error}");
        assert!(error.contains("sdk-component"), "{error}");
    }

    #[test]
    fn windows_path_enabled_keeps_extended_sdk_members_available() {
        let root = tempfile::tempdir().unwrap();
        let deep = (0..18).fold(root.path().to_path_buf(), |path, _| {
            path.join("sdk-component")
        });
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("header.h"), b"// SDK header").unwrap();
        validate_extraction_tree(root.path(), false).unwrap();
        assert!(deep.join("header.h").exists());
    }

    #[test]
    fn test_unsupported_format() {
        let result = extract(Path::new("file.xyz"), Path::new("/tmp"));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("unsupported archive format"));
    }

    /// Helper: create a minimal .tar.gz archive containing a single file.
    fn make_tar_gz(dest: &Path, entry_name: &str, content: &[u8]) {
        use flate2::Compression;
        use flate2::write::GzEncoder;

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
    fn streamed_manifest_matches_real_extracted_tree_across_formats() {
        let root = tempfile::tempdir().unwrap();
        for extension in ["tar.gz", "tar.xz", "zip"] {
            let archive = root.path().join(format!("test.{extension}"));
            match extension {
                "tar.gz" => make_tar_gz(&archive, "nested/file", b"streamed payload"),
                "tar.xz" => make_tar_xz(&archive, "nested/file", b"streamed payload"),
                _ => make_zip(&archive, "nested/file", b"streamed payload"),
            }
            let dest = root.path().join(extension);
            std::fs::create_dir(&dest).unwrap();
            let streamed = extract_with_manifest(&archive, &dest)
                .unwrap()
                .finish(&dest);
            if fbuild_core::platform::fs::executable_bit(&std::fs::metadata(&dest).unwrap())
                .is_some()
            {
                let streamed = streamed.unwrap();
                assert_eq!(
                    streamed.content_id(),
                    fbuild_core::content_manifest::hash_tree(&dest)
                        .unwrap()
                        .content_id()
                );
            } else {
                assert_eq!(
                    streamed.unwrap_err().kind(),
                    std::io::ErrorKind::Unsupported
                );
                assert_eq!(
                    std::fs::read(dest.join("nested/file")).unwrap(),
                    b"streamed payload"
                );
            }
        }
    }

    #[test]
    fn zip_without_mode_metadata_has_readable_default_permissions() {
        let root = tempfile::tempdir().unwrap();
        if fbuild_core::platform::fs::executable_bit(&std::fs::metadata(root.path()).unwrap())
            .is_none()
        {
            return;
        }
        let archive = root.path().join("dos.zip");
        make_zip(&archive, "file", b"payload");
        let mut bytes = std::fs::read(&archive).unwrap();
        let central = bytes.windows(4).position(|b| b == b"PK\x01\x02").unwrap();
        bytes[central + 38..central + 42].fill(0);
        std::fs::write(&archive, &bytes).unwrap();
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        assert!(zip.by_index(0).unwrap().unix_mode().is_none());
        let dest = root.path().join("out");
        std::fs::create_dir(&dest).unwrap();
        let manifest = extract_with_manifest(&archive, &dest)
            .unwrap()
            .finish(&dest)
            .unwrap();
        assert_eq!(std::fs::read(dest.join("file")).unwrap(), b"payload");
        assert!(!manifest.entries[0].exec);
        assert_eq!(
            manifest.content_id(),
            fbuild_core::content_manifest::hash_tree(&dest)
                .unwrap()
                .content_id()
        );
    }

    #[test]
    fn streamed_tar_defers_readonly_directories_and_clears_duplicate_exec() {
        use flate2::{Compression, write::GzEncoder};
        let root = tempfile::tempdir().unwrap();
        if fbuild_core::platform::fs::executable_bit(&std::fs::metadata(root.path()).unwrap())
            .is_none()
        {
            return;
        }
        let archive_path = root.path().join("fixture.tar.gz");
        let mut archive = tar::Builder::new(GzEncoder::new(
            std::fs::File::create(&archive_path).unwrap(),
            Compression::default(),
        ));
        let mut directory = tar::Header::new_gnu();
        directory.set_entry_type(tar::EntryType::Directory);
        directory.set_mode(0o555);
        directory.set_size(0);
        directory.set_cksum();
        archive
            .append_data(&mut directory, "readonly", &b""[..])
            .unwrap();
        for (mode, bytes) in [(0o755, b"old"), (0o644, b"new")] {
            let mut header = tar::Header::new_gnu();
            header.set_mode(mode);
            header.set_size(3);
            header.set_cksum();
            archive
                .append_data(&mut header, "readonly/file", &bytes[..])
                .unwrap();
        }
        let mut link = tar::Header::new_gnu();
        link.set_entry_type(tar::EntryType::Symlink);
        link.set_mode(0o777);
        link.set_size(0);
        link.set_link_name("readonly/file").unwrap();
        link.set_cksum();
        archive.append_data(&mut link, "link", &b""[..]).unwrap();
        archive.into_inner().unwrap().finish().unwrap();
        let dest = root.path().join("out");
        std::fs::create_dir(&dest).unwrap();
        let manifest = extract_with_manifest(&archive_path, &dest)
            .unwrap()
            .finish(&dest)
            .unwrap();
        assert_eq!(std::fs::read(dest.join("readonly/file")).unwrap(), b"new");
        assert!(
            !manifest
                .entries
                .iter()
                .find(|e| e.path == "readonly/file")
                .unwrap()
                .exec
        );
        assert_eq!(
            manifest.content_id(),
            fbuild_core::content_manifest::hash_tree(&dest)
                .unwrap()
                .content_id()
        );
        fbuild_core::platform::fs::set_archive_mode(&dest.join("readonly"), 0o755).unwrap();
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
