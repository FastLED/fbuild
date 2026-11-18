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
use std::process::Command;

#[path = "src/build_stamp.rs"]
mod build_stamp;

fn main() {
    // `cargo:rerun-if-changed` on the ref/index files is what actually makes
    // the stamp follow HEAD: the build script reruns when either moves, and a
    // changed `rustc-env` value recompiles `fbuild-core` and relinks its
    // dependents. `git status` cannot be watched that way — the dirty flag is
    // therefore as fresh as the last HEAD/index movement, which is the case
    // that matters (a *different build*, not an in-place edit; in-place asset
    // edits are covered by the assets digest below).
    //
    // The watched files are resolved from the checkout's `.git` itself, so a
    // linked worktree watches its own HEAD/index and the branch ref in the
    // shared git dir, and a packed branch watches `packed-refs`
    // (FastLED/fbuild#2036, #2042). Paths are absolute: cargo resolves
    // relative ones against this crate's directory.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let manifest_dir = Path::new(&manifest_dir);
    if let Some(layout) = build_stamp::find_git_layout(manifest_dir) {
        for watched in build_stamp::git_watch_paths(&layout) {
            println!("cargo:rerun-if-changed={}", watched.display());
        }
    }
    stamp_embedded_assets(manifest_dir);
    println!("cargo:rerun-if-changed=build.rs");

    let commit = run_git(&["rev-parse", "--short=12", "HEAD"]).unwrap_or_else(unknown);
    let dirty = match run_git(&["status", "--porcelain", "--untracked-files=no"]) {
        Some(porcelain) => !porcelain.is_empty(),
        None => false,
    };

    println!("cargo:rustc-env=FBUILD_BUILD_COMMIT={commit}");
    println!(
        "cargo:rustc-env=FBUILD_BUILD_DIRTY={}",
        if dirty { "1" } else { "0" }
    );
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

/// Placeholder used when the checkout cannot be fingerprinted.
fn unknown() -> String {
    "unknown".to_string()
}

/// Run `git -C <crate dir> <args>` and return trimmed stdout, or `None` when
/// git is absent, the directory is not a repository, or the command fails.
fn run_git(args: &[&str]) -> Option<String> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    // `fbuild-core`'s subprocess wrappers cannot be linked into a build
    // script, and this probe never runs at runtime.
    // allow-direct-spawn: compile-time git stamp; build scripts link no fbuild crates.
    let output = Command::new("git")
        .arg("-C")
        .arg(&manifest_dir)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
