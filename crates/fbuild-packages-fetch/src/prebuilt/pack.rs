//! Publisher-side packing of a prebuilt core-object tree (#1882).
//!
//! This is the *manual artifact builder* half of the track: the client never
//! runs it, a maintainer does, once, from a tree whose fingerprint is already
//! known. Nothing here uploads anything — decision 8c makes every bucket write
//! a maintainer action — so the tool's whole output is one archive plus the
//! release entry that describes it, written into a directory the caller names.
//!
//! The three properties that make the output servable are:
//!
//! - **determinism.** Entries are emitted in sorted order with normalized
//!   metadata (mode from the executable bit, uid/gid 0, empty uname/gname,
//!   mtime 0), so packing the same bytes twice on two hosts yields one digest —
//!   the digest a shard pins cannot depend on who ran the tool;
//! - **one shape.** The archive name, the bucket key and the entry's `flavor`
//!   and `provides` are all derived from `(product, reference, chip,
//!   fingerprint)`. A publisher cannot emit a key the client will not look up;
//! - **nothing unservable.** A tree containing a symlink, a foreign filesystem
//!   object, an empty tree, or a name that is not a plain relative path is
//!   refused before a byte is written, because [`extract`](super::extract)
//!   would refuse that archive and the failure would only appear at a user's
//!   first build.
use super::{Asset, reader::allowed_url, valid_sha};
use fbuild_core::path::NormalizedPath;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, io::Write, path::Path};

/// The `<product>` core objects are published under (decision 2's key space,
/// distinct from the vendor's own `esp32-arduino-libs` slices).
pub const DEFAULT_PRODUCT: &str = "arduino-esp32-core";
/// zstd level of the single-frame archive (decision 2).
pub const ZSTD_LEVEL: i32 = 19;
/// Refuse to pack an objects tree larger than this.
pub const MAX_OBJECTS_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// The only channel names decision 2 defines.
const CHANNELS: [&str; 2] = ["release", "nightly"];

#[derive(Debug, thiserror::Error)]
pub enum PackError {
    #[error("invalid pack request: {0}")]
    Invalid(&'static str),
    #[error("{path}: {error}")]
    Io { path: String, error: String },
    #[error("{0:?} is not a safe relative path, so it cannot be served")]
    Entry(String),
    #[error("{0:?} is a {1}, which prebuilt v1 archives never carry")]
    EntryType(String, &'static str),
    #[error("the objects tree holds nothing to publish")]
    Empty,
    #[error("the objects tree exceeds the {limit} byte limit")]
    TooLarge { limit: u64 },
    #[error("archive could not be written: {0}")]
    Archive(String),
    #[error("packed {path} hashes to {actual}, not the {expected} that was written")]
    Unstable {
        path: String,
        expected: String,
        actual: String,
    },
}

/// Everything the builder needs to name and describe one archive.
pub struct PackRequest<'a> {
    /// Compiled core-object tree to publish.
    pub objects: &'a Path,
    /// Directory the archive and its entry are written into.
    pub out: &'a Path,
    /// Bucket key vendor segment, e.g. `espressif`.
    pub vendor: &'a str,
    /// Product segment; [`DEFAULT_PRODUCT`] unless a maintainer rules otherwise.
    pub product: &'a str,
    /// `release` or `nightly`.
    pub channel: &'a str,
    /// Upstream ref verbatim, e.g. `3.3.12`.
    pub reference: &'a str,
    /// Chip (MCU) the objects were compiled for.
    pub chip: &'a str,
    /// Framework name the client queries with, e.g. `arduino-esp32`.
    pub framework: &'a str,
    /// 64-hex fingerprint of the built core objects (`Esp32Framework::prebuilt_core_plan`).
    pub fingerprint: &'a str,
    /// Client platform arch, e.g. `esp32`.
    pub arch: &'a str,
    /// Client platform ABI, e.g. `gcc@14.2.0`.
    pub abi: &'a str,
    /// Base URL the key is served under, without a trailing slash.
    pub base_url: &'a str,
    /// Oldest client that may read this entry (v1 is 1).
    pub min_client_version: u32,
}

/// One written archive, and the key it will be uploaded under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packed {
    pub archive: NormalizedPath,
    pub sha256: String,
    pub size_bytes: u64,
    pub entries: u64,
    pub key: String,
}

impl Packed {
    /// The archive's file name: the key's last segment.
    #[must_use]
    pub fn filename(&self) -> &str {
        self.key.rsplit('/').next().unwrap_or(&self.key)
    }

    /// Absolute URL list an entry carries, in mirror order (one here).
    #[must_use]
    pub fn urls(&self, base_url: &str) -> Vec<String> {
        vec![format!("{}/{}", base_url.trim_end_matches('/'), self.key)]
    }
}

/// The `tool` value the client uses for a framework's Catalog shard.
#[must_use]
pub fn tool_for(framework: &str) -> String {
    format!("prebuilt-{framework}")
}

/// The `fp-<first 16 hex>` flavor an entry must declare for `fingerprint`.
#[must_use]
pub fn flavor_for(fingerprint: &str) -> Option<String> {
    valid_sha(fingerprint).then(|| format!("fp-{}", &fingerprint[..16]))
}

/// Decision 2's key for one core-object archive.
#[must_use]
pub fn bucket_key(request: &PackRequest<'_>, sha256: &str) -> String {
    let name = format!("{}-{}-{}", request.product, request.reference, request.chip);
    format!(
        "v1/{}/{}/{}/{}/{chip}/{name}.{}.tar.zst",
        request.vendor,
        request.product,
        request.channel,
        request.reference,
        &sha256[..16],
        chip = request.chip,
    )
}

/// Pack `request.objects` into `request.out`, deterministically.
///
/// The archive is written to a sibling `.part` file, re-hashed from disk, and
/// only then renamed into place, so a run that dies leaves no file a publisher
/// could upload by mistake.
pub fn pack(request: &PackRequest<'_>) -> Result<Packed, PackError> {
    validate(request)?;
    let entries = collect(request.objects)?;
    std::fs::create_dir_all(request.out).map_err(|error| io_error(request.out, &error))?;
    let scratch =
        NormalizedPath::new(request.out).join(format!(".pack-{}.part", &request.fingerprint[..16]));
    let (sha256, size_bytes) = write_archive(&entries, scratch.as_path())?;
    let key = bucket_key(request, &sha256);
    let archive = NormalizedPath::new(request.out).join(key.rsplit('/').next().unwrap_or("packed"));
    commit(&scratch, &archive, &sha256)?;
    Ok(Packed {
        archive,
        sha256,
        size_bytes,
        entries: entries.len() as u64,
        key,
    })
}

/// The release entry that advertises `packed`, exactly as the client reads it.
#[must_use]
pub fn release(request: &PackRequest<'_>, packed: &Packed) -> Release {
    Release {
        tool: tool_for(request.framework),
        version: request.reference.to_string(),
        min_client_version: request.min_client_version,
        platforms: vec![ReleasePlatform {
            platform: Platform {
                os: "none".to_string(),
                arch: request.arch.to_string(),
                abi: request.abi.to_string(),
            },
            variant: Variant {
                flavor: flavor_for(request.fingerprint).unwrap_or_default(),
            },
            asset: Asset {
                filename: packed.filename().to_string(),
                size_bytes: packed.size_bytes,
                sha256: packed.sha256.clone(),
                urls: packed.urls(request.base_url),
                provides: vec![format!("fbuild-fp:{}", request.fingerprint)],
                parts: Vec::new(),
            },
        }],
    }
}

/// The Catalog shard holding `releases` for one tool.
#[must_use]
pub fn catalog(tool: &str, releases: Vec<Release>) -> Catalog {
    let channels = releases
        .iter()
        .map(|release| (release.version.clone(), release.version.clone()))
        .collect::<BTreeMap<_, _>>();
    Catalog {
        kind: "Catalog",
        schema_version: 1,
        tool: tool.to_string(),
        channels,
        releases,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Catalog {
    kind: &'static str,
    schema_version: u32,
    tool: String,
    channels: BTreeMap<String, String>,
    releases: Vec<Release>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Release {
    tool: String,
    version: String,
    min_client_version: u32,
    platforms: Vec<ReleasePlatform>,
}

#[derive(Debug, Clone, Serialize)]
struct ReleasePlatform {
    platform: Platform,
    variant: Variant,
    asset: Asset,
}

#[derive(Debug, Clone, Serialize)]
struct Platform {
    os: String,
    arch: String,
    abi: String,
}

#[derive(Debug, Clone, Serialize)]
struct Variant {
    flavor: String,
}

fn validate(request: &PackRequest<'_>) -> Result<(), PackError> {
    let fields = [
        (request.vendor, "vendor"),
        (request.product, "product"),
        (request.channel, "channel"),
        (request.reference, "reference"),
        (request.chip, "chip"),
        (request.framework, "framework"),
        (request.arch, "arch"),
        (request.abi, "abi"),
    ];
    for (value, name) in fields {
        if value.trim().is_empty() {
            return Err(PackError::Invalid(name));
        }
        if value.contains(['/', '\\']) || value.contains("..") {
            return Err(PackError::Invalid(name));
        }
    }
    if !CHANNELS.contains(&request.channel) {
        return Err(PackError::Invalid("channel"));
    }
    if !valid_sha(request.fingerprint) {
        return Err(PackError::Invalid("fingerprint"));
    }
    if request.min_client_version == 0 {
        return Err(PackError::Invalid("min_client_version"));
    }
    if !allowed_url(request.base_url) {
        return Err(PackError::Invalid("base_url"));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Dir,
    File,
}

struct Object {
    name: String,
    source: NormalizedPath,
    kind: Kind,
    exec: bool,
}

fn collect(objects: &Path) -> Result<Vec<Object>, PackError> {
    let root = NormalizedPath::new(objects);
    if !root.as_path().is_dir() {
        return Err(PackError::Invalid("objects"));
    }
    let mut found = Vec::new();
    let mut bytes = 0_u64;
    let walk = walkdir::WalkDir::new(root.as_path()).follow_links(false);
    for entry in walk {
        let entry = entry.map_err(|error| PackError::Io {
            path: error.path().unwrap_or(objects).display().to_string(),
            error: error.to_string(),
        })?;
        if entry.depth() == 0 {
            continue;
        }
        let path = NormalizedPath::new(entry.path());
        let name = path
            .relative_to(&root)
            .map(|relative| relative.display_slash())
            .ok_or_else(|| PackError::Entry(path.display_slash()))?;
        let display = name.clone();
        let metadata = std::fs::symlink_metadata(entry.path())
            .map_err(|error| io_error(entry.path(), &error))?;
        let kind = if metadata.is_dir() {
            Kind::Dir
        } else if metadata.is_file() {
            bytes = bytes.saturating_add(metadata.len());
            if bytes > MAX_OBJECTS_BYTES {
                return Err(PackError::TooLarge {
                    limit: MAX_OBJECTS_BYTES,
                });
            }
            Kind::File
        } else if metadata.is_symlink() {
            return Err(PackError::EntryType(display, "symlink"));
        } else {
            return Err(PackError::EntryType(display, "special file"));
        };
        found.push(Object {
            name,
            source: path,
            kind,
            exec: fbuild_core::platform::fs::is_executable(&metadata),
        });
    }
    if !found.iter().any(|object| object.kind == Kind::File) {
        return Err(PackError::Empty);
    }
    found.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(found)
}

/// Writes through a sha256 tee, so the digest covers exactly the bytes on disk.
struct HashingWriter<W: Write> {
    inner: W,
    hash: Sha256,
    bytes: u64,
}

impl<W: Write> HashingWriter<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            hash: Sha256::new(),
            bytes: 0,
        }
    }

    fn finish(mut self) -> Result<(W, String, u64), PackError> {
        self.inner
            .flush()
            .map_err(|error| PackError::Archive(error.to_string()))?;
        let digest = format!("{:x}", self.hash.finalize());
        Ok((self.inner, digest, self.bytes))
    }
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let written = self.inner.write(buf)?;
        self.hash.update(&buf[..written]);
        self.bytes = self.bytes.saturating_add(written as u64);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn write_archive(entries: &[Object], scratch: &Path) -> Result<(String, u64), PackError> {
    let file = std::fs::File::create(scratch).map_err(|error| io_error(scratch, &error))?;
    let hashing = HashingWriter::new(std::io::BufWriter::new(file));
    let encoder = zstd::stream::write::Encoder::new(hashing, ZSTD_LEVEL)
        .map_err(|error| PackError::Archive(error.to_string()))?;
    let mut archive = tar::Builder::new(encoder);
    for object in entries {
        append(&mut archive, object)?;
    }
    let encoder = archive
        .into_inner()
        .map_err(|error| PackError::Archive(error.to_string()))?;
    let hashing = encoder
        .finish()
        .map_err(|error| PackError::Archive(error.to_string()))?;
    let (buffered, sha256, size_bytes) = hashing.finish()?;
    let file = buffered
        .into_inner()
        .map_err(|error| PackError::Archive(error.error().to_string()))?;
    file.sync_all().map_err(|error| io_error(scratch, &error))?;
    Ok((sha256, size_bytes))
}

fn append<W: Write>(archive: &mut tar::Builder<W>, object: &Object) -> Result<(), PackError> {
    let mut header = tar::Header::new_gnu();
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_username("").ok();
    header.set_groupname("").ok();
    match object.kind {
        Kind::Dir => {
            header.set_entry_type(tar::EntryType::Directory);
            header.set_mode(0o755);
            header.set_size(0);
            header.set_cksum();
            let name = format!("{}/", object.name);
            archive
                .append_data(&mut header, name, std::io::empty())
                .map_err(|error| PackError::Archive(error.to_string()))
        }
        Kind::File => {
            header.set_entry_type(tar::EntryType::Regular);
            header.set_mode(if object.exec { 0o755 } else { 0o644 });
            let metadata = std::fs::metadata(object.source.as_path())
                .map_err(|error| io_error(object.source.as_path(), &error))?;
            header.set_size(metadata.len());
            header.set_cksum();
            let mut source = std::fs::File::open(object.source.as_path())
                .map_err(|error| io_error(object.source.as_path(), &error))?;
            archive
                .append_data(&mut header, &object.name, &mut source)
                .map_err(|error| PackError::Archive(error.to_string()))
        }
    }
}

/// Re-hash the packed file and only then move it into place.
fn commit(scratch: &Path, archive: &NormalizedPath, expected: &str) -> Result<(), PackError> {
    let bytes = std::fs::read(scratch).map_err(|error| io_error(scratch, &error))?;
    let actual = format!("{:x}", Sha256::digest(&bytes));
    if actual != expected {
        return Err(PackError::Unstable {
            path: archive.display_slash(),
            expected: expected.to_string(),
            actual,
        });
    }
    std::fs::rename(scratch, archive.as_path()).map_err(|error| io_error(archive.as_path(), &error))
}

fn io_error(path: &Path, error: &std::io::Error) -> PackError {
    PackError::Io {
        path: path.display().to_string(),
        error: error.to_string(),
    }
}

#[cfg(test)]
#[path = "pack_tests.rs"]
mod tests;
