//! The opt-in call site that turns an exact identity into one reusable tree
//! (#1882).
//!
//! The reader resolves, [`Fetcher`] verifies and [`extract`](super::extract)
//! unpacks — but nothing decided *where* a tree lives, or what to do when that
//! place is already taken. This cell owns exactly that decision:
//!
//! - the destination is derived from the exact identity alone, in the same
//!   layout the bucket uses (`prebuilt/<framework>/<version>/<arch>/fp-<sha16>`),
//!   so an archive and the tree it produced are found by one key;
//! - every key component is validated as a single ordinary path component, so a
//!   catalog value can never steer the write outside the cache root;
//! - a destination already carrying this archive's marker is reused with no
//!   download at all — the check runs before the fetcher is asked;
//! - a destination carrying a *different* archive's marker is refused and
//!   reported untouched: eviction belongs to the disk-cache LRU, and a live
//!   build may be reading that tree;
//! - extraction stages beside the destination, at a depth
//!   [`sweep_staging_tree`](super::sweep_staging_tree) visits, so an abandoned
//!   attempt is reaped by the GC cell.
//!
//! Opt-in: no build path calls this yet. The caller supplies the [`Fetcher`],
//! whose own destination decides where verified archives are kept.
use super::{
    Asset, Existing, ExtractError, ExtractedTree, FetchError, Fetcher, Query, existing_tree,
    extract_async, valid_sha,
};
use fbuild_core::path::NormalizedPath;
use std::path::Path;

/// Directory prefix of a tree keyed by fingerprint, matching the bucket key.
pub const FP_DIR_PREFIX: &str = "fp-";
/// Identity fields that become one directory name each, in this order.
const KEY_FIELDS: [&str; 3] = ["framework", "version", "arch"];

#[derive(Debug, thiserror::Error)]
pub enum ObtainError {
    #[error("prebuilt fingerprint must be 64 lowercase hex, got {0:?}")]
    InvalidFingerprint(String),
    #[error("prebuilt identity {field} {value:?} is not one ordinary path component")]
    InvalidComponent { field: &'static str, value: String },
    #[error(
        "the tree at {path} came from archive {existing:?}, not {expected}; it is left untouched"
    )]
    IdentityConflict {
        path: String,
        /// Digest of the tree already there; `None` when its marker is unreadable.
        existing: Option<String>,
        expected: String,
    },
    #[error(transparent)]
    Fetch(#[from] FetchError),
    #[error(transparent)]
    Extract(#[from] ExtractError),
}

/// One tree, and how this call obtained it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Obtained {
    pub tree: ExtractedTree,
    /// True when this call downloaded and extracted; false when it reused a
    /// tree that was already complete.
    pub downloaded: bool,
}

/// Where the tree for `query` lives: `{cache}/prebuilt/<fw>/<version>/<arch>/fp-<sha16>`.
///
/// The bucket key built by [`operator_urls`](super::operator_urls) is the same
/// relative path, so a tree and the archive it came from share one identity.
pub fn tree_root(cache_root: &Path, query: &Query) -> Result<NormalizedPath, ObtainError> {
    if !valid_sha(&query.fp) {
        return Err(ObtainError::InvalidFingerprint(query.fp.clone()));
    }
    let values = [&query.framework, &query.version, &query.arch];
    let mut root = NormalizedPath::new(crate::disk_cache::paths::prebuilt_root(cache_root));
    for (field, value) in KEY_FIELDS.iter().zip(values) {
        root = root.join(component(field, value)?);
    }
    Ok(root.join(format!("{FP_DIR_PREFIX}{}", &query.fp[..16])))
}

/// Reuse the tree already at the identity's path, or fetch and extract it there.
///
/// The marker is consulted first, so a complete tree costs no network traffic
/// and a conflicting tree costs no download either.
pub async fn obtain(
    cache_root: &Path,
    query: &Query,
    asset: &Asset,
    fetcher: &Fetcher,
) -> Result<Obtained, ObtainError> {
    let dest = tree_root(cache_root, query)?;
    match existing_tree(dest.as_path(), &asset.sha256, asset.size_bytes) {
        Existing::Same(tree) => {
            return Ok(Obtained {
                tree,
                downloaded: false,
            });
        }
        Existing::Different(existing) => {
            return Err(ObtainError::IdentityConflict {
                path: dest.display_slash(),
                existing,
                expected: asset.sha256.clone(),
            });
        }
        Existing::None => {}
    }
    let artifact = fetcher.fetch(asset).await?;
    let tree = extract_async(artifact, dest.as_path()).await?;
    Ok(Obtained {
        tree,
        downloaded: true,
    })
}

/// One key component: non-empty, not `.`/`..`, and no separator, drive or NUL.
fn component<'a>(field: &'static str, value: &'a str) -> Result<&'a str, ObtainError> {
    let bad =
        value.is_empty() || value == "." || value == ".." || value.contains(['/', '\\', ':', '\0']);
    if bad {
        return Err(ObtainError::InvalidComponent {
            field,
            value: value.to_string(),
        });
    }
    Ok(value)
}

#[cfg(test)]
#[path = "obtain_tests.rs"]
mod tests;
