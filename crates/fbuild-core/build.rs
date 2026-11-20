//! Stamp the embedded-assets digest into `fbuild-core` (FastLED/fbuild#2042).
//!
//! The no-op build fingerprint keys on the bytes of the assets compiled into
//! the workspace (config JSONs, board files, scripts), because the commit
//! stamp cannot see an uncommitted edit. The digest changes only when those
//! bytes do, so it is safe this low in the crate graph.
//!
//! The commit stamp is deliberately *not* here. It changes on every commit,
//! and stamped into `fbuild-core` it recompiled all 19 workspace crates that
//! depend on it, so every hosted clippy, test and Dylint pass rechecked the
//! whole workspace whatever the commit touched. `fbuild-cli` and
//! `fbuild-daemon`, at the top of the graph, stamp it instead
//! (`commit_stamp.rs`, FastLED/fbuild#1742).

use std::path::Path;

#[path = "src/build_stamp.rs"]
mod build_stamp;

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let manifest_dir = Path::new(&manifest_dir);
    stamp_embedded_assets(manifest_dir);
    println!("cargo:rerun-if-changed=build.rs");
}

/// Hash every asset the workspace crates compile in and watch each one, so
/// an edited config JSON reruns this script and changes the stamp.
fn stamp_embedded_assets(manifest_dir: &Path) {
    let Some(crates_dir) = manifest_dir.parent() else {
        return;
    };
    let assets = build_stamp::embedded_assets(crates_dir);
    for asset in assets.files.iter().chain(&assets.asset_dirs) {
        println!("cargo:rerun-if-changed={}", asset.path.display());
    }
    let digest = build_stamp::assets_digest(&assets.files);
    println!("cargo:rustc-env=FBUILD_EMBEDDED_ASSETS_DIGEST={digest}");
}
