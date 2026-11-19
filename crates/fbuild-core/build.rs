//! Stamp the workspace build identity into `fbuild-core` (FastLED/fbuild#1742).
//!
//! Two builds of the same workspace version used to be indistinguishable:
//! `fbuild --version` and `fbuild daemon status` both printed `2.5.46`, so an
//! editable install that kept yesterday's binaries — or a daemon left running
//! from an older checkout — looked current, and a missing feature was blamed on
//! fbuild instead of on the stale binary (#1742, and the daemon confusion in
//! #1739).
//!
//! The commit and dirty flag are baked in at compile time and reported by both
//! binaries. `fbuild-core` carries the stamp for the whole workspace: every
//! crate that reports a version depends on it, so a single probe keeps the CLI
//! and the daemon agreeing instead of duplicating git logic per binary.
//!
//! The probe is best-effort by design. A missing `git`, a source tarball with
//! no `.git`, or a repository owned by another user must never fail a build —
//! the stamp degrades to `unknown`, which the CLI prints verbatim.
//!
//! It also stamps a digest of the assets compiled into the workspace (config
//! JSONs, board files, scripts). The commit stamp cannot see an uncommitted
//! edit, so the no-op build fingerprint keys on these bytes too
//! (FastLED/fbuild#2042).

use std::path::Path;

#[path = "src/build_stamp.rs"]
mod build_stamp;
#[path = "commit_stamp.rs"]
mod commit_stamp;

fn main() {
    // The commit stamp and its git watch paths: see `commit_stamp::emit`.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let manifest_dir = Path::new(&manifest_dir);
    commit_stamp::emit(manifest_dir);
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
