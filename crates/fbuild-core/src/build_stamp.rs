//! Embedded-assets digest for `build.rs` (FastLED/fbuild#2042).
//!
//! `build.rs` includes this file with `#[path]`, and the library compiles it
//! only under `cfg(test)` so the helpers get unit tests. Build scripts cannot
//! link fbuild crates, so this file uses `std` only.
//!
//! Config JSONs and other assets are compiled in
//!   with `include_str!` / `include_dir!`. Their bytes feed the no-op build
//!   fingerprint, so an edit relinks even in a dirty dev checkout, where the
//!   commit stamp does not move.

use std::path::Path;

/// An embedded asset file or directory.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Asset {
    /// Path relative to the crates dir, `/`-joined so it is the same on every
    /// host.
    pub name: String,
    pub path: Box<Path>,
}

/// Embedded assets under `crates_dir`, and what to watch for them.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct EmbeddedAssets {
    /// Every asset file, sorted by name.
    pub files: Vec<Asset>,
    /// Topmost directories holding assets and no Rust source, so an added or
    /// removed asset reruns the build script while a `.rs` edit does not.
    pub asset_dirs: Vec<Asset>,
}

/// Every non-Rust, non-Markdown file under each crate's `src/` and `assets/`
/// directories in `crates_dir`: the config JSONs, board files and scripts the
/// crates compile in.
pub fn embedded_assets(crates_dir: &Path) -> EmbeddedAssets {
    let mut assets = EmbeddedAssets::default();
    for crate_dir in sorted_entries(crates_dir, "") {
        for root in ["src", "assets"] {
            let dir = Asset {
                name: format!("{}/{root}", crate_dir.name),
                path: crate_dir.path.join(root).into_boxed_path(),
            };
            if dir.path.is_dir() {
                let scan = collect_assets(&dir, &mut assets);
                if scan.has_asset && !scan.has_rust {
                    assets.asset_dirs.push(dir);
                }
            }
        }
    }
    assets.files.sort();
    assets.asset_dirs.sort();
    assets
}

/// What [`collect_assets`] found below a directory.
#[derive(Clone, Copy)]
struct Scan {
    has_rust: bool,
    has_asset: bool,
}

/// Collect assets below `dir` and record its topmost asset-only subdirectories.
fn collect_assets(dir: &Asset, assets: &mut EmbeddedAssets) -> Scan {
    let mut scan = Scan {
        has_rust: false,
        has_asset: false,
    };
    let mut asset_only = Vec::new();
    for entry in sorted_entries(&dir.path, &dir.name) {
        if entry.path.is_dir() {
            let child = collect_assets(&entry, assets);
            scan.has_rust |= child.has_rust;
            scan.has_asset |= child.has_asset;
            if child.has_asset && !child.has_rust {
                asset_only.push(entry);
            }
        } else if entry.path.extension().is_some_and(|ext| ext == "rs") {
            scan.has_rust = true;
        } else if is_asset(&entry.path) {
            scan.has_asset = true;
            assets.files.push(entry);
        }
    }
    // A directory without Rust source is watched whole by its parent (cargo
    // scans a watched directory recursively), so only a mixed directory
    // records its asset-only children.
    if scan.has_rust {
        assets.asset_dirs.extend(asset_only);
    }
    scan
}

fn is_asset(path: &Path) -> bool {
    !path
        .extension()
        .is_some_and(|ext| ext == "rs" || ext == "md")
}

/// The entries of `dir`, sorted, named below `prefix`.
fn sorted_entries(dir: &Path, prefix: &str) -> Vec<Asset> {
    let mut entries: Vec<Asset> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let file_name = entry.file_name().to_string_lossy().into_owned();
            let name = if prefix.is_empty() {
                file_name
            } else {
                format!("{prefix}/{file_name}")
            };
            Some(Asset {
                name,
                path: entry.path().into_boxed_path(),
            })
        })
        .collect();
    entries.sort();
    entries
}

/// SHA-256 over each asset's name and bytes. An unreadable file hashes as
/// empty rather than failing the build.
pub fn assets_digest(files: &[Asset]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for file in files {
        let bytes = std::fs::read(&file.path).unwrap_or_default();
        hasher.update((file.name.len() as u64).to_le_bytes());
        hasher.update(file.name.as_bytes());
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
    }
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
#[path = "build_stamp_tests.rs"]
mod tests;
