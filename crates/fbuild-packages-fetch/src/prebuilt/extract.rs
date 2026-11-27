//! Streaming, integrity-checked extraction of a verified prebuilt archive
//! (#1882).
//!
//! [`extract`] turns an [`Artifact`] into a populated tree and never trusts
//! the archive's layout:
//!
//! - the archive is `.tar.zst` only (the v1 bucket shape); anything else is
//!   refused instead of guessed at by extension;
//! - the archive is read **once**: its declared `size_bytes` and SHA-256 are
//!   recomputed while the tar entries are decompressed, so a body that was
//!   swapped after [`Fetcher`](super::Fetcher) verified it cannot land;
//! - every entry must be a relative path of ordinary components and either a
//!   regular file or a directory — absolute paths, `..`, symlinks and hard
//!   links are refused, so an archive cannot write outside the tree;
//! - the decompressed size and entry count are bounded, so a small archive
//!   cannot fill the disk;
//! - extraction goes into a staging directory next to the destination and is
//!   moved into place only after the marker is written, so a killed process
//!   leaves either a complete tree or nothing claimable.
//!
//! A destination that already carries a matching marker is returned as-is
//! without touching the archive; a destination that holds anything else is
//! refused, never merged into. Eviction is the staging/GC and LRU cells of
//! #1882, not this one.
use super::{Artifact, valid_sha};
use fbuild_core::path::NormalizedPath;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Component, Path};
use std::sync::atomic::{AtomicU64, Ordering};

/// Refuse an archive that would decompress to more than this.
pub const MAX_EXTRACTED_BYTES: u64 = 16 * 1024 * 1024 * 1024;
/// Refuse an archive with more entries than this.
pub const MAX_ENTRIES: u64 = 200_000;
/// Completion marker: present only in a tree that finished verifying.
pub const MARKER: &str = ".prebuilt-tree.json";

#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    #[error("invalid artifact: {0}")]
    Invalid(&'static str),
    #[error("prebuilt v1 serves .tar.zst only, got {0}")]
    Unsupported(String),
    #[error(
        "archive does not match its descriptor: sha256 {actual} of {read} bytes, expected {expected} of {declared}"
    )]
    Integrity {
        expected: String,
        actual: String,
        declared: u64,
        read: u64,
    },
    #[error("archive entry {0:?} is not a safe relative path")]
    Entry(String),
    #[error("archive entry {0:?} is a {1}, which prebuilt v1 does not extract")]
    EntryType(String, &'static str),
    #[error("archive would extract more than the {limit} byte limit")]
    TooLarge { limit: u64 },
    #[error("archive holds more than {limit} entries")]
    TooManyEntries { limit: u64 },
    #[error("destination {path} already holds a tree this artifact did not produce")]
    Occupied { path: String },
    #[error("{path}: {error}")]
    Io { path: String, error: String },
    #[error("archive is not readable as a tar.zst stream: {0}")]
    Archive(String),
    #[error("extraction task failed: {0}")]
    Join(String),
}

/// A completed tree on disk, produced from exactly one verified archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedTree {
    pub root: NormalizedPath,
    pub entries: u64,
    pub bytes: u64,
}

/// Marker written last inside a tree, so a marker implies a full extraction.
///
/// `size_bytes` is the archive's size (what the descriptor pins) and
/// `extracted_bytes` the bytes the tree holds; a reuse must report the same
/// numbers a fresh extraction reports, so both are recorded.
#[derive(Debug, Serialize, Deserialize)]
struct TreeMarker {
    sha256: String,
    size_bytes: u64,
    entries: u64,
    #[serde(default)]
    extracted_bytes: u64,
}

/// What asking "does this destination already hold the tree for this archive?"
/// found. Extraction only reports this; the call site decides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Existing {
    /// Nothing claimable at the destination.
    None,
    /// The marker of exactly this archive: the tree can be reused as-is.
    Same(ExtractedTree),
    /// A tree, or a marker that does not parse, that this archive did not
    /// produce. The digest is named when the marker could be read.
    Different(Option<String>),
}

/// Report whether `dest` already holds the tree for `(sha256, size_bytes)`.
///
/// Total on purpose: an absent or unreadable marker is [`Existing::None`] (an
/// occupied directory is still refused later by [`extract`]), and a marker that
/// does not parse is [`Existing::Different`] — never "assume it is ours".
pub fn existing_tree(dest: &Path, sha256: &str, size_bytes: u64) -> Existing {
    let dest = NormalizedPath::new(dest);
    let Ok(bytes) = std::fs::read(dest.join(MARKER).as_path()) else {
        return Existing::None;
    };
    let Ok(marker) = serde_json::from_slice::<TreeMarker>(&bytes) else {
        return Existing::Different(None);
    };
    if marker.sha256 != sha256 || marker.size_bytes != size_bytes {
        return Existing::Different(Some(marker.sha256));
    }
    Existing::Same(ExtractedTree {
        root: dest,
        entries: marker.entries,
        bytes: marker.extracted_bytes,
    })
}

/// Extract `artifact` into `dest`, or return the tree already there.
pub fn extract(artifact: &Artifact, dest: &Path) -> Result<ExtractedTree, ExtractError> {
    extract_with_limit(artifact, dest, MAX_EXTRACTED_BYTES)
}

/// [`extract`] with an explicit decompressed-size ceiling (tests use a small one).
pub fn extract_with_limit(
    artifact: &Artifact,
    dest: &Path,
    limit: u64,
) -> Result<ExtractedTree, ExtractError> {
    if !valid_sha(&artifact.sha256) {
        return Err(ExtractError::Invalid("sha256 must be 64 lowercase hex"));
    }
    let dest = NormalizedPath::new(dest);
    if let Some(tree) = completed(&dest, artifact)? {
        return Ok(tree);
    }
    require_supported(artifact)?;
    if exists(&dest) && !(is_dir(&dest)? && is_empty(&dest)?) {
        return Err(ExtractError::Occupied {
            path: dest.display_slash(),
        });
    }
    let staging = Staging::new(&dest, &artifact.sha256)?;
    let result = unpack(artifact, staging.path(), limit)?;
    write_marker(staging.path(), artifact, &result)?;
    staging.commit(&dest)?;
    Ok(ExtractedTree {
        root: dest,
        entries: result.entries,
        bytes: result.bytes,
    })
}

/// Blocking extraction on its own thread, for callers on the daemon runtime.
pub async fn extract_async(artifact: Artifact, dest: &Path) -> Result<ExtractedTree, ExtractError> {
    let dest = NormalizedPath::new(dest);
    tokio::task::spawn_blocking(move || extract(&artifact, dest.as_path()))
        .await
        .map_err(|error| ExtractError::Join(error.to_string()))?
}

fn require_supported(artifact: &Artifact) -> Result<(), ExtractError> {
    // The artifact was renamed to `<sha16>-<name>` by the fetcher, so the
    // extension decision reads the file it will actually open.
    let name = artifact
        .path
        .as_path()
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if name.to_lowercase().ends_with(".tar.zst") {
        Ok(())
    } else {
        Err(ExtractError::Unsupported(name))
    }
}

/// Return the tree when the destination already holds this exact archive.
fn completed(
    dest: &NormalizedPath,
    artifact: &Artifact,
) -> Result<Option<ExtractedTree>, ExtractError> {
    match existing_tree(dest.as_path(), &artifact.sha256, artifact.size_bytes) {
        Existing::None => Ok(None),
        Existing::Same(tree) => Ok(Some(tree)),
        Existing::Different(_) => Err(ExtractError::Occupied {
            path: dest.display_slash(),
        }),
    }
}

struct Counted {
    entries: u64,
    bytes: u64,
}

/// Extract every entry into `root`, verifying the archive as it streams.
fn unpack(artifact: &Artifact, root: &Path, limit: u64) -> Result<Counted, ExtractError> {
    let file = std::fs::File::open(artifact.path.as_path())
        .map_err(|error| io_error(artifact.path.as_path(), error))?;
    // The hasher wraps the *compressed* file, so the digest is over exactly the
    // bytes the descriptor describes; the decoder sits above it.
    let decoder = zstd::stream::read::Decoder::new(HashingReader::new(file))
        .map_err(|error| ExtractError::Archive(error.to_string()))?;
    let mut archive = tar::Archive::new(decoder);
    let mut counted = Counted {
        entries: 0,
        bytes: 0,
    };
    let entries = archive
        .entries()
        .map_err(|error| ExtractError::Archive(error.to_string()))?;
    for entry in entries {
        let mut entry = entry.map_err(|error| ExtractError::Archive(error.to_string()))?;
        counted.entries += 1;
        if counted.entries > MAX_ENTRIES {
            return Err(ExtractError::TooManyEntries { limit: MAX_ENTRIES });
        }
        let size = write_entry(&mut entry, root)?;
        counted.bytes = counted.bytes.saturating_add(size);
        if counted.bytes > limit {
            return Err(ExtractError::TooLarge { limit });
        }
    }
    // A tar reader stops at the end-of-archive blocks; drain the rest of the
    // compressed stream so the digest covers the whole file, not a prefix.
    let mut stream = archive.into_inner();
    std::io::copy(&mut stream, &mut std::io::sink())
        .map_err(|error| ExtractError::Archive(error.to_string()))?;
    let (actual, read) = stream.finish().into_inner().finish();
    if actual != artifact.sha256 || read != artifact.size_bytes {
        return Err(ExtractError::Integrity {
            expected: artifact.sha256.clone(),
            actual,
            declared: artifact.size_bytes,
            read,
        });
    }
    Ok(counted)
}

/// Create one entry under `root`, returning the bytes it declares.
fn write_entry<R: Read>(entry: &mut tar::Entry<'_, R>, root: &Path) -> Result<u64, ExtractError> {
    let path = entry
        .path()
        .map_err(|error| ExtractError::Archive(error.to_string()))?
        .to_path_buf();
    let shown = path.display().to_string();
    let relative = safe_relative(&path)?;
    let kind = entry.header().entry_type();
    if kind.is_dir() {
        let target = root.join(relative);
        std::fs::create_dir_all(&target).map_err(|error| io_error(&target, error))?;
        return Ok(0);
    }
    if !kind.is_file() {
        return Err(ExtractError::EntryType(shown, describe(kind)));
    }
    let declared = entry
        .header()
        .size()
        .map_err(|error| ExtractError::Archive(error.to_string()))?;
    let target = root.join(relative);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|error| io_error(parent, error))?;
    }
    let mut file = std::fs::File::create(&target).map_err(|error| io_error(&target, error))?;
    let executable = entry.header().mode().is_ok_and(|mode| mode & 0o111 != 0);
    let written = std::io::copy(entry, &mut file).map_err(|error| io_error(&target, error))?;
    if written != declared {
        return Err(ExtractError::Archive(format!(
            "{shown}: stored {written} of {declared} bytes"
        )));
    }
    // `File::create` cannot carry the archive's execute bits, and a tree that
    // lost them is not the tree the publisher fingerprinted. Only the execute
    // bits are applied - never setuid/setgid, whatever the header claims.
    if executable {
        fbuild_core::platform::fs::set_executable(&target)
            .map_err(|error| io_error(&target, error))?;
    }
    Ok(written)
}

/// Reject anything that could land outside `root`. `PathBuf` is banned by the
/// path dylints, so this borrows the header's path.
fn safe_relative(path: &Path) -> Result<&Path, ExtractError> {
    let empty = path.as_os_str().is_empty();
    let escapes = path
        .components()
        .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir));
    if empty || escapes {
        return Err(ExtractError::Entry(path.display().to_string()));
    }
    Ok(path)
}

fn describe(kind: tar::EntryType) -> &'static str {
    match kind {
        tar::EntryType::Symlink => "symlink",
        tar::EntryType::Link => "hard link",
        tar::EntryType::Char => "character device",
        tar::EntryType::Block => "block device",
        tar::EntryType::Fifo => "fifo",
        _ => "special entry",
    }
}

fn write_marker(root: &Path, artifact: &Artifact, counted: &Counted) -> Result<(), ExtractError> {
    let marker = TreeMarker {
        sha256: artifact.sha256.clone(),
        size_bytes: artifact.size_bytes,
        entries: counted.entries,
        extracted_bytes: counted.bytes,
    };
    let bytes =
        serde_json::to_vec(&marker).map_err(|error| ExtractError::Archive(error.to_string()))?;
    let path = root.join(MARKER);
    std::fs::write(&path, bytes).map_err(|error| io_error(&path, error))
}

fn exists(path: &NormalizedPath) -> bool {
    std::fs::symlink_metadata(path.as_path()).is_ok()
}

fn is_dir(path: &NormalizedPath) -> Result<bool, ExtractError> {
    let metadata = std::fs::symlink_metadata(path.as_path())
        .map_err(|error| io_error(path.as_path(), error))?;
    Ok(metadata.is_dir())
}

fn is_empty(path: &NormalizedPath) -> Result<bool, ExtractError> {
    let mut read =
        std::fs::read_dir(path.as_path()).map_err(|error| io_error(path.as_path(), error))?;
    Ok(read.next().is_none())
}

fn io_error(path: &Path, error: std::io::Error) -> ExtractError {
    ExtractError::Io {
        path: path.display().to_string(),
        error: error.to_string(),
    }
}

/// A staging directory that is removed unless it was committed.
struct Staging {
    path: NormalizedPath,
    committed: bool,
}

static ATTEMPT: AtomicU64 = AtomicU64::new(0);

impl Staging {
    fn new(dest: &NormalizedPath, sha: &str) -> Result<Self, ExtractError> {
        let parent = dest
            .as_path()
            .parent()
            .ok_or(ExtractError::Invalid("destination has no parent directory"))?;
        std::fs::create_dir_all(parent).map_err(|error| io_error(parent, error))?;
        let attempt = ATTEMPT.fetch_add(1, Ordering::Relaxed);
        let path = NormalizedPath::new(parent).join(format!(".{}.{}.staging", &sha[..16], attempt));
        if exists(&path) {
            std::fs::remove_dir_all(path.as_path())
                .map_err(|error| io_error(path.as_path(), error))?;
        }
        std::fs::create_dir_all(path.as_path()).map_err(|error| io_error(path.as_path(), error))?;
        Ok(Self {
            path,
            committed: false,
        })
    }

    fn path(&self) -> &Path {
        self.path.as_path()
    }

    fn commit(mut self, dest: &NormalizedPath) -> Result<(), ExtractError> {
        if exists(dest) {
            // An empty destination is the only thing we replace; anything with
            // contents was refused before staging began.
            std::fs::remove_dir(dest.as_path()).map_err(|error| io_error(dest.as_path(), error))?;
        }
        std::fs::rename(self.path.as_path(), dest.as_path())
            .map_err(|error| io_error(dest.as_path(), error))?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_dir_all(self.path.as_path());
        }
    }
}

/// Hashes and counts the compressed archive while the tar reader pulls from it.
struct HashingReader<R> {
    inner: R,
    hasher: Sha256,
    read: u64,
}

impl<R> HashingReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            read: 0,
        }
    }

    fn finish(self) -> (String, u64) {
        (format!("{:x}", self.hasher.finalize()), self.read)
    }
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.hasher.update(&buf[..read]);
        self.read += read as u64;
        Ok(read)
    }
}

#[cfg(test)]
#[path = "extract_tests.rs"]
mod tests;
