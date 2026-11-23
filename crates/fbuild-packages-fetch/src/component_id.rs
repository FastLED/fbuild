//! Per-component identity stamps for prebuilt framework artifacts.
//!
//! Design: `docs/design/artifact-serving-decisions.md` §8a (FastLED/fbuild#1882).
//! Framework identity is per component and is taken **in the component's staging
//! directory**, never as a hash of an installed tree. A component writes one
//! stamp inside its own subtree, so it is either absent or complete with its
//! identity; a tree installed before this landed has no stamp and therefore no
//! identity, and the planner compiles from source instead of guessing.

use std::io::Read;
use std::path::Path;

use fbuild_core::path::NormalizedPath;
use fbuild_core::{FbuildError, Result, platform::fs};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Stamp file, written at the component's root.
pub const STAMP_FILE: &str = "component-identity.json";
/// Schema of [`ComponentIdentity`]. A stamp carrying any other schema value has
/// no identity: an older or newer writer is not this reader's contract.
pub const SCHEMA: u32 = 1;

/// Domain tag, so a component id can never collide with another digest in the
/// workspace that happens to hash the same bytes.
const DOMAIN: &[u8] = b"fbuild-component-identity\0";
/// Install sentinel written after the atomic rename (`disk_cache::paths`). It is
/// never part of a staging tree, but excluding it keeps an id taken over an
/// installed tree equal to the id taken over the staging tree it came from.
const INSTALL_SENTINEL: &str = ".install_complete";

/// One component's identity, written into that component's subtree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentIdentity {
    /// [`SCHEMA`] at write time.
    pub schema: u32,
    /// Component name: `core`, `sdk-<variant>`, `mcu-<variant>`.
    pub component: String,
    /// Component version, from the package version the tree was installed for.
    pub version: String,
    /// Content id over the component's files (see [`content_id`]).
    pub content_id: String,
    /// Bumped by any post-extract mutation of the component's own subtree, so a
    /// tree mutated after it was stamped cannot present the same identity.
    pub recipe: u32,
}

impl ComponentIdentity {
    /// Identity for a component whose staging tree is final.
    #[must_use]
    pub fn new(component: &str, version: &str, content_id: String, recipe: u32) -> Self {
        Self {
            schema: SCHEMA,
            component: component.to_string(),
            version: version.to_string(),
            content_id,
            recipe,
        }
    }
}

/// Hash one component's files: relative path, bytes, exec bit, symlink target.
///
/// `excluded` are component-root-relative subtrees that belong to *another*
/// component and are therefore invisible here (decision 8a: the core excludes
/// the SDK trees it merges into later).
///
/// Directories carry no identity of their own, so an empty directory is
/// invisible; every file and symlink below `root` is included.
pub fn content_id(root: &Path, excluded: &[&str]) -> Result<String> {
    let root = NormalizedPath::new(root);
    if !root.as_path().is_dir() {
        return Err(FbuildError::PackageError(format!(
            "cannot take a component identity: {} is not a directory",
            root.display()
        )));
    }
    let excluded: Vec<String> = excluded
        .iter()
        .map(|name| NormalizedPath::new(name).display_slash())
        .collect();
    let mut hasher = Sha256::new();
    hasher.update(DOMAIN);
    let walk = walkdir::WalkDir::new(root.as_path())
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|entry| !is_excluded(entry.path(), &root, &excluded));
    for entry in walk {
        let entry = entry.map_err(|error| {
            FbuildError::PackageError(format!(
                "could not read component tree {}: {error}",
                root.display()
            ))
        })?;
        let Some(relative) = relative_key(entry.path(), &root) else {
            continue;
        };
        if relative == STAMP_FILE || relative == INSTALL_SENTINEL {
            continue;
        }
        let metadata = entry.metadata().map_err(|error| {
            FbuildError::PackageError(format!(
                "could not read component entry {relative}: {error}"
            ))
        })?;
        let file_type = metadata.file_type();
        if file_type.is_dir() {
            continue;
        }
        if file_type.is_symlink() {
            let target = std::fs::read_link(entry.path()).map_err(|error| {
                FbuildError::PackageError(format!(
                    "could not read component symlink {relative}: {error}"
                ))
            })?;
            hasher.update(b"l\0");
            hasher.update(relative.as_bytes());
            hasher.update(b"\0");
            hasher.update(NormalizedPath::new(&target).display_slash().as_bytes());
            hasher.update(b"\0");
            continue;
        }
        if !file_type.is_file() {
            // Devices and reparse points are not part of a component's content.
            continue;
        }
        hasher.update(b"f\0");
        hasher.update(relative.as_bytes());
        hasher.update(b"\0");
        hasher.update([u8::from(fs::is_executable(&metadata))]);
        hasher.update(metadata.len().to_le_bytes());
        hash_file(entry.path(), &relative, &mut hasher)?;
    }
    Ok(hex(&hasher.finalize()))
}

/// Write `identity` at `root`.
pub fn write_stamp(root: &Path, identity: &ComponentIdentity) -> Result<()> {
    let path = NormalizedPath::new(root).join(STAMP_FILE);
    let bytes = serde_json::to_vec_pretty(identity).map_err(|error| {
        FbuildError::PackageError(format!("could not encode component identity: {error}"))
    })?;
    std::fs::write(path.as_path(), bytes).map_err(|error| {
        FbuildError::PackageError(format!(
            "could not write component identity to {}: {error}",
            path.display()
        ))
    })
}

/// Read the stamp at `root`, or `None` when the component has no identity: no
/// stamp, an unreadable one, or one written under another schema.
#[must_use]
pub fn read_stamp(root: &Path) -> Option<ComponentIdentity> {
    let path = NormalizedPath::new(root).join(STAMP_FILE);
    let bytes = std::fs::read(path.as_path()).ok()?;
    let identity: ComponentIdentity = serde_json::from_slice(&bytes).ok()?;
    (identity.schema == SCHEMA).then_some(identity)
}

/// Remove the stamp at `root`, if any.
///
/// Used when the bytes a stamp described did not survive a later step: the
/// component then has no identity and is rebuilt from source. Idempotent - a
/// component that already has no identity is not an error.
pub fn remove_stamp(root: &Path) -> Result<()> {
    let path = NormalizedPath::new(root).join(STAMP_FILE);
    match std::fs::remove_file(path.as_path()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(FbuildError::PackageError(format!(
            "could not remove the component identity at {}: {error}",
            path.display()
        ))),
    }
}

fn hash_file(path: &Path, relative: &str, hasher: &mut Sha256) -> Result<()> {
    let mut file = std::fs::File::open(path).map_err(|error| {
        FbuildError::PackageError(format!("could not read component file {relative}: {error}"))
    })?;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            FbuildError::PackageError(format!("could not read component file {relative}: {error}"))
        })?;
        if read == 0 {
            return Ok(());
        }
        hasher.update(&buffer[..read]);
    }
}

fn is_excluded(path: &Path, root: &NormalizedPath, excluded: &[String]) -> bool {
    let Some(relative) = relative_key(path, root) else {
        return false;
    };
    excluded.iter().any(|exclude| {
        relative == *exclude
            || (relative.len() > exclude.len()
                && relative.starts_with(exclude.as_str())
                && relative.as_bytes()[exclude.len()] == b'/')
    })
}

fn relative_key(path: &Path, root: &NormalizedPath) -> Option<String> {
    let relative = NormalizedPath::new(path).relative_to(root)?;
    let key = relative.display_slash();
    (!key.is_empty()).then_some(key)
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}
