//! Content identity for executable images when their modification times disagree.

use std::io;
use std::path::Path;

#[cfg(unix)]
use std::io::Write;

#[cfg(unix)]
const MEMO_HEADER: &str = "fbuild-image-hash v1";

/// Hash a binary on demand. Normal daemon probes compare mtimes without
/// opening the image; this is only used to resolve a newer-mtime mismatch.
pub fn blake3_file(path: &Path) -> io::Result<blake3::Hash> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    io::copy(&mut file, &mut hasher)?;
    Ok(hasher.finalize())
}

/// Reuse a digest across short-lived CLI processes while the executable's
/// file identity is unchanged. A stale or unwritable memo only costs a rehash.
#[cfg(unix)]
pub fn memoized_blake3_file(path: &Path, memo_dir: &Path) -> io::Result<blake3::Hash> {
    let before = file_identity(path)?;
    let name = blake3::hash(path.as_os_str().as_encoded_bytes());
    let memo = memo_dir.join(format!("{}.identity", &name.to_hex().as_str()[..32]));
    if let Ok(contents) = std::fs::read_to_string(&memo) {
        let mut lines = contents.lines();
        if lines.next() == Some(MEMO_HEADER) && lines.next() == Some(before.as_str()) {
            let hash = lines
                .next()
                .and_then(|hex| blake3::Hash::from_hex(hex).ok());
            if lines.next().is_none()
                && file_identity(path).ok().as_deref() == Some(before.as_str())
            {
                if let Some(hash) = hash {
                    return Ok(hash);
                }
            }
        }
    }

    let mut file = std::fs::File::open(path)?;
    if metadata_identity(&file.metadata()?)? != before {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "executable changed before hashing",
        ));
    }
    let mut hasher = blake3::Hasher::new();
    io::copy(&mut file, &mut hasher)?;
    let hash = hasher.finalize();
    if file_identity(path)? != before {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "executable changed while hashing",
        ));
    }
    let _ = write_memo(memo_dir, &memo, &before, hash);
    Ok(hash)
}

/// Windows metadata does not provide a reliable change-time key for this
/// memo, so hash the image directly when the mtime check asks for it.
#[cfg(not(unix))]
pub fn memoized_blake3_file(path: &Path, _memo_dir: &Path) -> io::Result<blake3::Hash> {
    blake3_file(path)
}

#[cfg(unix)]
fn file_identity(path: &Path) -> io::Result<String> {
    metadata_identity(&path.metadata()?)
}

#[cfg(unix)]
fn metadata_identity(metadata: &std::fs::Metadata) -> io::Result<String> {
    use std::os::unix::fs::MetadataExt;
    let modified = metadata
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok(format!(
        "{} {} {} {} {}.{}",
        metadata.len(),
        modified.as_nanos(),
        metadata.dev(),
        metadata.ino(),
        metadata.ctime(),
        metadata.ctime_nsec()
    ))
}

#[cfg(unix)]
fn write_memo(dir: &Path, path: &Path, identity: &str, hash: blake3::Hash) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut temporary = tempfile::NamedTempFile::new_in(dir)?;
    write!(temporary, "{MEMO_HEADER}\n{identity}\n{}\n", hash.to_hex())?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn memo_reuses_equal_bytes_and_invalidates_after_replacement() {
        let root = tempfile::tempdir().unwrap();
        let image = root.path().join("image");
        let memo_dir = root.path().join("memo");
        std::fs::write(&image, b"first image").unwrap();
        let first = memoized_blake3_file(&image, &memo_dir).unwrap();
        assert_eq!(first, blake3::hash(b"first image"));
        assert_eq!(memoized_blake3_file(&image, &memo_dir).unwrap(), first);

        // Replacement must invalidate the memo even if the byte count matches.
        let replacement = root.path().join("replacement");
        std::fs::write(&replacement, b"other image").unwrap();
        std::fs::rename(&replacement, &image).unwrap();
        let second = memoized_blake3_file(&image, &memo_dir).unwrap();
        assert_eq!(second, blake3::hash(b"other image"));
        assert_ne!(second, first);
    }
}
