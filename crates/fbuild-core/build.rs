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

use std::process::Command;

fn main() {
    // `cargo:rerun-if-changed` on the ref/index files is what actually makes
    // the stamp follow HEAD: the build script reruns when either moves, and a
    // changed `rustc-env` value recompiles `fbuild-core` and relinks its
    // dependents. `git status` cannot be watched that way — the dirty flag is
    // therefore as fresh as the last HEAD/index movement, which is the case
    // that matters (a *different build*, not an in-place edit).
    for relative in ["../.git/HEAD", "../.git/index"] {
        if std::path::Path::new(relative).exists() {
            println!("cargo:rerun-if-changed={relative}");
        }
    }
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
