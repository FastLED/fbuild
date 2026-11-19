//! Tests for the git-layout helpers of `commit_stamp.rs` (FastLED/fbuild#2036, #2042).

use std::path::Path;

use super::*;

fn write(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

/// The contents of every watched file, so a test can say which git file is
/// watched without comparing differently spelled paths.
fn watched_contents(layout: &GitLayout) -> Vec<String> {
    git_watch_paths(layout)
        .iter()
        .filter(|path| path.is_file())
        .map(|path| std::fs::read_to_string(path).unwrap())
        .collect()
}

/// A repository at `root/main` on branch `topic`, with a loose ref.
fn main_repo(root: &Path) -> Box<Path> {
    let git = root.join("main").join(".git");
    write(&git.join("HEAD"), "ref: refs/heads/topic\n");
    write(&git.join("index"), "main index");
    write(&git.join("refs/heads/topic"), "main topic ref");
    git.into_boxed_path()
}

/// A linked worktree of `main` at `root/wt`, checked out on `branch`.
fn worktree(root: &Path, git: &Path, gitdir_line: &str, branch: &str) -> Box<Path> {
    let admin = git.join("worktrees").join("wt");
    write(&admin.join("HEAD"), &format!("ref: refs/heads/{branch}\n"));
    write(&admin.join("index"), "worktree index");
    write(&admin.join("commondir"), "../..\n");
    let checkout = root.join("wt");
    write(&checkout.join(".git"), gitdir_line);
    write(&checkout.join("crates/fbuild-core/build.rs"), "");
    checkout.join("crates/fbuild-core").into_boxed_path()
}

#[test]
fn gitdir_file_is_parsed() {
    assert_eq!(
        parse_gitdir_file("gitdir: /repo/.git/worktrees/wt\n"),
        Some("/repo/.git/worktrees/wt")
    );
    assert_eq!(parse_gitdir_file("gitdir:../x"), Some("../x"));
    assert_eq!(parse_gitdir_file("gitdir:   \n"), None);
    assert_eq!(parse_gitdir_file("ref: refs/heads/main"), None);
}

#[test]
fn a_plain_checkout_watches_head_index_and_the_loose_branch_ref() {
    let tmp = tempfile::TempDir::new().unwrap();
    main_repo(tmp.path());
    let start = tmp.path().join("main/crates/fbuild-core");
    std::fs::create_dir_all(&start).unwrap();

    let layout = find_git_layout(&start).unwrap();
    assert_eq!(layout.git_dir, layout.common_dir);
    let watched = watched_contents(&layout);
    assert!(watched.contains(&"ref: refs/heads/topic\n".to_string()));
    assert!(watched.contains(&"main index".to_string()));
    assert!(watched.contains(&"main topic ref".to_string()));
}

/// In a worktree HEAD and the index are per checkout, but the branch ref is
/// in the shared git dir named by `commondir`.
#[test]
fn a_worktree_watches_its_own_head_and_the_shared_branch_ref() {
    let tmp = tempfile::TempDir::new().unwrap();
    let git = main_repo(tmp.path());
    write(&git.join("refs/heads/feature"), "shared feature ref");
    let admin = git.join("worktrees").join("wt");
    let start = worktree(
        tmp.path(),
        &git,
        &format!("gitdir: {}\n", admin.display()),
        "feature",
    );

    let layout = find_git_layout(&start).unwrap();
    let watched = watched_contents(&layout);
    assert!(watched.contains(&"ref: refs/heads/feature\n".to_string()));
    assert!(watched.contains(&"worktree index".to_string()));
    assert!(
        watched.contains(&"shared feature ref".to_string()),
        "the branch ref lives in the common dir; got {watched:?}"
    );
    assert!(!watched.contains(&"main index".to_string()));
}

#[test]
fn a_relative_gitdir_resolves_against_the_checkout() {
    let tmp = tempfile::TempDir::new().unwrap();
    let git = main_repo(tmp.path());
    write(&git.join("refs/heads/feature"), "shared feature ref");
    let start = worktree(
        tmp.path(),
        &git,
        "gitdir: ../main/.git/worktrees/wt\n",
        "feature",
    );

    let watched = watched_contents(&find_git_layout(&start).unwrap());
    assert!(watched.contains(&"shared feature ref".to_string()));
}

/// After `git pack-refs` the branch exists only in `packed-refs`; the next
/// commit writes a loose ref into the watched `refs/heads` directory.
#[test]
fn a_packed_branch_watches_packed_refs_and_the_ref_directory() {
    let tmp = tempfile::TempDir::new().unwrap();
    let git = main_repo(tmp.path());
    write(&git.join("packed-refs"), "abc refs/heads/packed\n");
    let admin = git.join("worktrees").join("wt");
    let start = worktree(
        tmp.path(),
        &git,
        &format!("gitdir: {}\n", admin.display()),
        "packed",
    );

    let layout = find_git_layout(&start).unwrap();
    assert!(watched_contents(&layout).contains(&"abc refs/heads/packed\n".to_string()));
    let refs_dir = layout.common_dir.join("refs").join("heads");
    assert!(
        git_watch_paths(&layout)
            .iter()
            .any(|path| path.is_dir() && path.join("topic").is_file()),
        "{} must be watched for the next loose ref",
        refs_dir.display()
    );
}

#[test]
fn a_detached_head_watches_only_head_and_index() {
    let tmp = tempfile::TempDir::new().unwrap();
    let git = main_repo(tmp.path());
    write(&git.join("HEAD"), "0123456789abcdef\n");
    let layout = find_git_layout(&git.parent().unwrap().join("x")).unwrap();
    assert_eq!(git_watch_paths(&layout).len(), 2);
}
