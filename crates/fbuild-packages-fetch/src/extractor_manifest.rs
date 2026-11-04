//! Streaming package extraction feeding the portable content manifest.
use fbuild_core::{
    FbuildError, Result,
    content_manifest::{ManifestBuilder, portable_path},
};
use std::{
    fs,
    io::{self, Read},
    path::Path,
};

/// Extract safely, hashing regular files as they are written.
pub fn extract_with_manifest(archive: &Path, root: &Path) -> Result<ManifestBuilder> {
    let name = archive.to_string_lossy().to_ascii_lowercase();
    let file = fs::File::open(archive)?;
    let builder = if name.ends_with(".zip") {
        unzip(file, root)?
    } else {
        let reader: Box<dyn Read> = if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
            Box::new(flate2::read::GzDecoder::new(file))
        } else if name.ends_with(".tar.bz2") {
            Box::new(bzip2::read::BzDecoder::new(file))
        } else if name.ends_with(".tar.xz") || name.ends_with(".txz") {
            Box::new(xz2::read::XzDecoder::new(file))
        } else if name.ends_with(".tar.zst") {
            Box::new(zstd::Decoder::new(file)?)
        } else {
            return Err(FbuildError::PackageError(
                "unsupported archive format".into(),
            ));
        };
        untar(reader, root)?
    };
    super::extractor::validate_extraction(root)?;
    Ok(builder)
}

/// Reject symlink ancestors before opening an output, including archive-created links.
fn output_path(root: &Path, relative: &Path) -> io::Result<std::path::PathBuf> {
    let name = portable_path(relative)?;
    if name.is_empty() {
        return Err(io::Error::other("empty archive file path"));
    }
    let output = root.join(name);
    let mut ancestor = output.as_path();
    while ancestor != root {
        if fs::symlink_metadata(ancestor).is_ok_and(|m| m.is_symlink()) {
            return Err(io::Error::other("archive output traverses a symlink"));
        }
        ancestor = ancestor
            .parent()
            .ok_or_else(|| io::Error::other("unsafe archive output"))?;
    }
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(output)
}

fn untar(reader: impl Read, root: &Path) -> io::Result<ManifestBuilder> {
    let mut archive = tar::Archive::new(reader);
    let mut builder = ManifestBuilder::default();
    let mut links = Vec::new();
    let mut directories = Vec::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
        let relative = entry.path()?.into_owned();
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            if portable_path(&relative)?.is_empty() {
                continue;
            }
            let output = output_path(root, &relative)?;
            fs::create_dir_all(&output)?;
            directories.push((output, entry.header().mode()?));
        } else if kind.is_file() {
            let output = output_path(root, &relative)?;
            let mode = entry.header().mode()?;
            let exec = mode & 0o111 != 0;
            builder.copy(
                &portable_path(&relative)?,
                exec,
                &mut entry,
                &mut fs::File::create(&output)?,
            )?;
            fbuild_core::platform::fs::set_archive_mode(&output, mode)?;
            builder.record_metadata(&portable_path(&relative)?, &fs::metadata(&output)?)?;
        } else if kind.is_hard_link() {
            let target = entry
                .link_name()?
                .ok_or_else(|| io::Error::other("missing hardlink target"))?
                .into_owned();
            links.push((relative, target));
        } else {
            if !portable_path(&relative)?.is_empty() {
                output_path(root, &relative)?;
            }
            if !entry.unpack_in(root)? {
                return Err(io::Error::other("unsafe tar member"));
            }
        }
    }
    for (relative, target) in links {
        let output = output_path(root, &relative)?;
        let target = output_path(root, &target)?;
        fs::hard_link(target, output)?;
    }
    directories.sort_by_key(|(path, _)| std::cmp::Reverse(path.components().count()));
    for (output, mode) in directories {
        fbuild_core::platform::fs::set_archive_mode(&output, mode)?;
    }
    Ok(builder)
}

fn unzip(file: fs::File, root: &Path) -> io::Result<ManifestBuilder> {
    let mut archive = zip::ZipArchive::new(file)?;
    let mut builder = ManifestBuilder::default();
    let mut directories = Vec::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let relative = entry
            .enclosed_name()
            .ok_or_else(|| io::Error::other("unsafe zip member"))?;
        let output = output_path(root, &relative)?;
        if entry.is_dir() {
            fs::create_dir_all(&output)?;
            directories.push((output, entry.unix_mode().unwrap_or(0o755)));
            continue;
        }
        let mode = entry.unix_mode().unwrap_or(0o644);
        if mode & 0o170000 == 0o120000 {
            let mut target = String::new();
            entry.read_to_string(&mut target)?;
            fbuild_core::platform::fs::symlink_file(Path::new(&target), &output)?;
        } else {
            let exec = mode & 0o111 != 0;
            builder.copy(
                &portable_path(&relative)?,
                exec,
                &mut entry,
                &mut fs::File::create(&output)?,
            )?;
            fbuild_core::platform::fs::set_archive_mode(&output, mode)?;
            builder.record_metadata(&portable_path(&relative)?, &fs::metadata(&output)?)?;
        }
    }
    directories.sort_by_key(|(path, _)| std::cmp::Reverse(path.components().count()));
    for (output, mode) in directories {
        fbuild_core::platform::fs::set_archive_mode(&output, mode)?;
    }
    Ok(builder)
}
