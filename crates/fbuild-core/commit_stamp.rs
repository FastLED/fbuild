//! Commit stamp for the build id that `fbuild --version` and
//! `fbuild daemon status` report (FastLED/fbuild#1742).
//!
//! A `build.rs` includes this file with `#[path]`; `fbuild-core`'s library
//! compiles its git-layout helpers (not the probe) under `cfg(test)` for unit
//! tests. Build scripts cannot link fbuild crates, so this file uses `std`
//! only, and it lives outside `src/` because the probe spawns `git` directly.
//!
//! The probe is best-effort by design. A missing `git`, a source tarball with
//! no `.git`, or a repository owned by another user must never fail a build;
//! the stamp degrades to `unknown`, which the CLI prints verbatim.
//!
//! Git watch paths: cargo reruns the build script when a `rerun-if-changed`
//! file moves. The branch ref lives in the *common* git dir, which is not the
//! per-checkout git dir in a worktree (`.git` is a `gitdir:` file and
//! `commondir` names the shared dir), and a ref may exist only in
//! `packed-refs`.

use std::path::Path;
#[cfg(not(test))]
use std::process::Command;

/// The per-checkout git dir (HEAD, index) and the shared dir (refs).
#[derive(Debug, PartialEq, Eq)]
pub struct GitLayout {
    /// `.git`, or `<common>/worktrees/<name>` in a linked worktree.
    pub git_dir: Box<Path>,
    /// Where branch refs and `packed-refs` live. Equal to `git_dir` outside a
    /// linked worktree.
    pub common_dir: Box<Path>,
}

/// The path in a `.git` file (`gitdir: <path>`), as written.
pub fn parse_gitdir_file(contents: &str) -> Option<&str> {
    let path = contents.lines().next()?.strip_prefix("gitdir:")?.trim();
    (!path.is_empty()).then_some(path)
}

/// The git layout of the checkout containing `start`, found by walking up to
/// the nearest `.git`. `None` outside a checkout (e.g. a source tarball).
pub fn find_git_layout(start: &Path) -> Option<GitLayout> {
    let dot_git = start
        .ancestors()
        .map(|dir| dir.join(".git"))
        .find(|candidate| candidate.exists())?;
    let git_dir = if dot_git.is_dir() {
        dot_git
    } else {
        let contents = std::fs::read_to_string(&dot_git).ok()?;
        let target = parse_gitdir_file(&contents)?;
        dot_git.parent()?.join(target)
    };
    let common_dir = match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(contents) => git_dir.join(contents.trim()),
        Err(_) => git_dir.clone(),
    };
    Some(GitLayout {
        git_dir: git_dir.into_boxed_path(),
        common_dir: common_dir.into_boxed_path(),
    })
}

/// Existing files whose change can mean HEAD names a different commit.
///
/// HEAD and the index are per checkout. The branch ref is in the common dir:
/// its loose file when there is one, otherwise `packed-refs` plus the ref's
/// directory, so the loose file appearing on the next commit is seen. Only
/// existing paths are returned: cargo treats a missing one as always stale.
pub fn git_watch_paths(layout: &GitLayout) -> Vec<Box<Path>> {
    let head = layout.git_dir.join("HEAD");
    let mut paths = vec![head.clone(), layout.git_dir.join("index")];
    let branch = std::fs::read_to_string(&head)
        .ok()
        .and_then(|contents| Some(contents.strip_prefix("ref:")?.trim().to_string()));
    if let Some(branch) = branch {
        let loose = layout.common_dir.join(&branch);
        if loose.is_file() {
            paths.push(loose);
        } else {
            paths.push(layout.common_dir.join("packed-refs"));
            if let Some(parent) = loose.parent() {
                paths.push(parent.to_path_buf());
            }
        }
    }
    paths
        .into_iter()
        .filter(|path| path.exists())
        .map(|path| path.into_boxed_path())
        .collect()
}

/// Emit `FBUILD_BUILD_COMMIT` / `FBUILD_BUILD_DIRTY` and the git paths whose
/// movement must rerun the calling build script.
///
/// `cargo:rerun-if-changed` on the ref/index files is what makes the stamp
/// follow HEAD. `git status` cannot be watched that way, so the dirty flag is
/// as fresh as the last HEAD/index movement, which is the case that matters
/// (a *different build*, not an in-place edit; in-place asset edits are
/// covered by the assets digest). The watched files come from the checkout's
/// own `.git`, so a linked worktree watches its HEAD/index and the branch ref
/// in the shared git dir, and a packed branch watches `packed-refs`
/// (FastLED/fbuild#2036, #2042).
// Build scripts only: fbuild-core's tests compile this file from `src/`, where
// the spawn-discipline Dylint bans a raw `git` spawn. The fbuild-cli
// integration test covers the probe end to end through `fbuild --version`.
#[cfg(not(test))]
pub fn emit(manifest_dir: &Path) {
    if let Some(layout) = find_git_layout(manifest_dir) {
        for watched in git_watch_paths(&layout) {
            println!("cargo:rerun-if-changed={}", watched.display());
        }
    }
    let commit = run_git(manifest_dir, &["rev-parse", "--short=12", "HEAD"])
        .unwrap_or_else(|| "unknown".to_string());
    let dirty = run_git(
        manifest_dir,
        &["status", "--porcelain", "--untracked-files=no"],
    )
    .is_some_and(|porcelain| !porcelain.is_empty());
    println!("cargo:rustc-env=FBUILD_BUILD_COMMIT={commit}");
    println!(
        "cargo:rustc-env=FBUILD_BUILD_DIRTY={}",
        if dirty { "1" } else { "0" }
    );
}

/// `git -C <dir> <args>` trimmed stdout, or `None` when git is absent, the
/// directory is not a repository, or the command fails.
#[cfg(not(test))]
fn run_git(dir: &Path, args: &[&str]) -> Option<String> {
    // allow-direct-spawn: compile-time git stamp; build scripts link no fbuild crates.
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
#[path = "src/commit_stamp_tests.rs"]
mod tests;
