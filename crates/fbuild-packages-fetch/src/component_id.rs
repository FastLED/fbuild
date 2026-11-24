//! Per-component identity stamps for prebuilt framework artifacts.
//!
//! Design: `docs/design/artifact-serving-decisions.md` §8a (FastLED/fbuild#1882).
//! Framework identity is per component and is taken **in the component's staging
//! directory**, never as a hash of an installed tree. A component writes one
//! stamp inside its own subtree, so it is either absent or complete with its
//! identity; a tree installed before this landed has no stamp and therefore no
//! identity, and the planner compiles from source instead of guessing.
//!
//! Two components can share one directory: the MCU skeleton contributes its
//! entries under `tools/esp32-arduino-libs/<mcu>/`, which is the directory the
//! SDK variant of that chip owns. Stamps are therefore **one file per
//! component** ([`stamp_file_name`]), and a component merged into a directory
//! another one already populated is verified over the entries it contributed
//! ([`entries_survived`]) instead of over the whole merged tree.

use std::io::Read;
use std::path::Path;

use fbuild_core::path::NormalizedPath;
use fbuild_core::{FbuildError, Result, platform::fs};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Prefix of a component's stamp file: `<prefix><component>.json`.
pub const STAMP_PREFIX: &str = "component-identity.";
/// Suffix of a component's stamp file.
pub const STAMP_SUFFIX: &str = ".json";
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

/// Stamp file name for `component`, written at that component's root.
///
/// One file per component, so the SDK variant and the MCU skeleton of the same
/// chip - which both install into `tools/esp32-arduino-libs/<mcu>/` - keep
/// separate identities instead of overwriting each other's.
#[must_use]
pub fn stamp_file_name(component: &str) -> String {
    format!("{STAMP_PREFIX}{component}{STAMP_SUFFIX}")
}

/// Whether `relative`, a component-root-relative key, names a stamp file.
///
/// Stamps are only ever written at a component's root, so a nested file that
/// happens to carry the prefix is content, not identity.
#[must_use]
pub fn is_stamp_file(relative: &str) -> bool {
    !relative.contains('/')
        && relative.starts_with(STAMP_PREFIX)
        && relative.ends_with(STAMP_SUFFIX)
        && relative.len() > STAMP_PREFIX.len() + STAMP_SUFFIX.len()
}

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
/// invisible; every file and symlink below `root` is included, and the stamps of
/// every other component at `root` are not.
pub fn content_id(root: &Path, excluded: &[&str]) -> Result<String> {
    let root = NormalizedPath::new(root);
    if !root.as_path().is_dir() {
        return Err(FbuildError::PackageError(format!(
            "cannot take a component identity: {} is not a directory",
            root.display()
        )));
    }
    let excluded = excluded_keys(excluded);
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
        if is_stamp_file(&relative) || relative == INSTALL_SENTINEL {
            continue;
        }
        hash_entry(entry.path(), &relative, &mut hasher)?;
    }
    Ok(hex(&hasher.finalize()))
}

/// Whether every entry of `staged_root` reproduced byte for byte at the same
/// relative path under `installed_root`.
///
/// Equality of whole-tree ids cannot be the test where a component merges into a
/// directory that already holds another component's files: the MCU skeleton
/// copies its entries into the SDK variant's `tools/esp32-arduino-libs/<mcu>/`,
/// and on ESP32-C2 the core archive ships a partial tree of that same directory,
/// so the installed tree is a superset by design. This walks the staged tree and
/// compares the same paths on the installed side; a missing entry, a changed
/// byte, a flipped exec bit or a different link target all report `false`.
pub fn entries_survived(
    staged_root: &Path,
    installed_root: &Path,
    excluded: &[&str],
) -> Result<bool> {
    let staged_root = NormalizedPath::new(staged_root);
    if !staged_root.as_path().is_dir() {
        return Ok(false);
    }
    let installed_root = NormalizedPath::new(installed_root);
    if !installed_root.as_path().is_dir() {
        return Ok(false);
    }
    let excluded = excluded_keys(excluded);
    let mut staged = Sha256::new();
    let mut installed = Sha256::new();
    staged.update(DOMAIN);
    installed.update(DOMAIN);
    let walk = walkdir::WalkDir::new(staged_root.as_path())
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|entry| !is_excluded(entry.path(), &staged_root, &excluded));
    for entry in walk {
        let entry = entry.map_err(|error| {
            FbuildError::PackageError(format!(
                "could not read component tree {}: {error}",
                staged_root.display()
            ))
        })?;
        let Some(relative) = relative_key(entry.path(), &staged_root) else {
            continue;
        };
        if is_stamp_file(&relative) || relative == INSTALL_SENTINEL {
            continue;
        }
        let file_type = entry.file_type();
        if !file_type.is_file() && !file_type.is_symlink() {
            continue;
        }
        let counterpart = installed_root.join(&relative);
        if std::fs::symlink_metadata(counterpart.as_path()).is_err() {
            return Ok(false);
        }
        hash_entry(entry.path(), &relative, &mut staged)?;
        hash_entry(counterpart.as_path(), &relative, &mut installed)?;
    }
    Ok(staged.finalize() == installed.finalize())
}

/// Write `identity` at `root`, under its own component's file name.
pub fn write_stamp(root: &Path, identity: &ComponentIdentity) -> Result<()> {
    if identity.component.is_empty() || identity.component.contains('/') {
        return Err(FbuildError::PackageError(format!(
            "cannot write a component identity for {:?}: a component name is part of its stamp file name",
            identity.component
        )));
    }
    let path = NormalizedPath::new(root).join(stamp_file_name(&identity.component));
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

/// Read `component`'s stamp at `root`, or `None` when it has no identity: no
/// stamp, an unreadable one, one written under another schema, or one filed
/// under a different component's name.
#[must_use]
pub fn read_stamp(root: &Path, component: &str) -> Option<ComponentIdentity> {
    let path = NormalizedPath::new(root).join(stamp_file_name(component));
    let identity = read_stamp_file(path.as_path())?;
    (identity.component == component).then_some(identity)
}

/// Whether `root` carries at least one component identity, of any component.
///
/// Used where a tree must not be mutated in place: whichever component stamped
/// it, the bytes on disk are final.
#[must_use]
pub fn has_stamp(root: &Path) -> bool {
    let root = NormalizedPath::new(root);
    let Ok(entries) = std::fs::read_dir(root.as_path()) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let name = entry.file_name().to_string_lossy().into_owned();
        is_stamp_file(&name) && read_stamp_file(&entry.path()).is_some()
    })
}

/// Remove `component`'s stamp at `root`, if any.
///
/// Used when the bytes a stamp described did not survive a later step: the
/// component then has no identity and is rebuilt from source. Idempotent - a
/// component that already has no identity is not an error.
pub fn remove_stamp(root: &Path, component: &str) -> Result<()> {
    let path = NormalizedPath::new(root).join(stamp_file_name(component));
    match std::fs::remove_file(path.as_path()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(FbuildError::PackageError(format!(
            "could not remove the component identity at {}: {error}",
            path.display()
        ))),
    }
}

fn read_stamp_file(path: &Path) -> Option<ComponentIdentity> {
    let bytes = std::fs::read(path).ok()?;
    let identity: ComponentIdentity = serde_json::from_slice(&bytes).ok()?;
    (identity.schema == SCHEMA).then_some(identity)
}

/// Hash one file or symlink's contribution: a `f`/`l` tag, the relative key the
/// entry carries, and the bytes (plus exec bit and length) or the link target.
///
/// `relative` is the key the entry carries rather than one derived from `path`,
/// so the same bytes at two paths hash differently and a rename changes an id.
fn hash_entry(path: &Path, relative: &str, hasher: &mut Sha256) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        FbuildError::PackageError(format!(
            "could not read component entry {relative}: {error}"
        ))
    })?;
    let file_type = metadata.file_type();
    if file_type.is_dir() {
        return Ok(());
    }
    if file_type.is_symlink() {
        let target = std::fs::read_link(path).map_err(|error| {
            FbuildError::PackageError(format!(
                "could not read component symlink {relative}: {error}"
            ))
        })?;
        hasher.update(b"l\0");
        hasher.update(relative.as_bytes());
        hasher.update(b"\0");
        hasher.update(NormalizedPath::new(&target).display_slash().as_bytes());
        hasher.update(b"\0");
        return Ok(());
    }
    if !file_type.is_file() {
        // Devices and reparse points are not part of a component's content.
        return Ok(());
    }
    hasher.update(b"f\0");
    hasher.update(relative.as_bytes());
    hasher.update(b"\0");
    hasher.update([u8::from(fs::is_executable(&metadata))]);
    hasher.update(metadata.len().to_le_bytes());
    hash_file(path, relative, hasher)
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

fn excluded_keys(excluded: &[&str]) -> Vec<String> {
    excluded
        .iter()
        .map(|name| NormalizedPath::new(name).display_slash())
        .collect()
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

/// Domain tag for [`component_term`], so a framework term can never collide with
/// a component id that happens to hash the same bytes.
const TERM_DOMAIN: &[u8] = b"fbuild-framework-term\0";
/// Encoding of [`component_term`]. Bump it when what the term covers changes,
/// so a term computed by an older reader never matches a newer one.
pub const TERM_SCHEMA: u32 = 1;

/// Canonical term over the component identities of one framework tree.
///
/// This is the **framework term** of a prebuilt fingerprint (decision 8a): the
/// build already hashes its board, toolchain and flag metadata, and this adds
/// the bytes of the framework those flags point at. A term is
/// order-independent - components are sorted by name - and install-history
/// independent, because each component's id is taken over its own staging tree
/// and therefore never depends on which other chips the user has built.
///
/// Returns `None` when there is no identity to certify:
/// - an empty identity list (a framework with no stamps has no identity, and
///   the caller falls back to compiling from source rather than inventing one),
/// - two identities for the same component name (an ambiguous tree cannot
///   present a canonical single id).
#[must_use]
pub fn component_term(identities: &[ComponentIdentity]) -> Option<String> {
    if identities.is_empty() {
        return None;
    }
    let mut ordered: Vec<&ComponentIdentity> = identities.iter().collect();
    ordered.sort_by(|left, right| left.component.cmp(&right.component));
    if ordered
        .windows(2)
        .any(|pair| pair[0].component == pair[1].component)
    {
        return None;
    }
    let mut hasher = Sha256::new();
    hasher.update(TERM_DOMAIN);
    hasher.update(TERM_SCHEMA.to_le_bytes());
    hasher.update((ordered.len() as u64).to_le_bytes());
    for identity in ordered {
        for field in [
            identity.component.as_str(),
            identity.version.as_str(),
            identity.content_id.as_str(),
        ] {
            hasher.update(field.as_bytes());
            hasher.update([0u8]);
        }
        hasher.update(identity.recipe.to_le_bytes());
    }
    Some(hex(&hasher.finalize()))
}
