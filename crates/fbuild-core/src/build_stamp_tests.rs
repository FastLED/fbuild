//! Tests for the `build.rs` stamp helpers (FastLED/fbuild#2042).

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

/// A crates dir with configs, a board asset, Markdown and Rust source.
fn crates_tree(root: &Path) {
    write(&root.join("a/src/lib.rs"), "");
    write(&root.join("a/src/README.md"), "docs");
    write(
        &root.join("a/src/configs/avr.json"),
        r#"{"linker_flags":["-Wl,--gc-sections"]}"#,
    );
    write(&root.join("a/src/mixed/mod.rs"), "");
    write(&root.join("a/src/mixed/table.json"), "[]");
    write(&root.join("b/assets/boards/json/uno.json"), "{}");
    write(&root.join("b/data/fixture.bin"), "test only");
}

fn names(assets: &[Asset]) -> Vec<&str> {
    assets.iter().map(|asset| asset.name.as_str()).collect()
}

#[test]
fn embedded_assets_are_the_non_rust_files_under_src_and_assets() {
    let tmp = tempfile::TempDir::new().unwrap();
    crates_tree(tmp.path());
    let assets = embedded_assets(tmp.path());
    assert_eq!(
        names(&assets.files),
        [
            "a/src/configs/avr.json",
            "a/src/mixed/table.json",
            "b/assets/boards/json/uno.json"
        ]
    );
    // `src/mixed` holds Rust source, so watching it would rerun the build
    // script on every `.rs` edit; its asset is watched as a file instead.
    assert_eq!(names(&assets.asset_dirs), ["a/src/configs", "b/assets"]);
}

#[test]
fn the_assets_digest_follows_asset_bytes_and_names() {
    let tmp = tempfile::TempDir::new().unwrap();
    crates_tree(tmp.path());
    let digest = || assets_digest(&embedded_assets(tmp.path()).files);
    let base = digest();
    assert_eq!(base, digest(), "unchanged assets keep the digest");

    let config = tmp.path().join("a/src/configs/avr.json");
    write(&config, r#"{"linker_flags":["-Wl,--relax"]}"#);
    let edited = digest();
    assert_ne!(base, edited, "an edited config JSON must change the digest");

    std::fs::rename(&config, tmp.path().join("a/src/configs/avr2.json")).unwrap();
    assert_ne!(edited, digest(), "a renamed asset must change the digest");

    write(&tmp.path().join("a/src/lib.rs"), "fn edited() {}");
    write(&tmp.path().join("a/src/README.md"), "edited docs");
    let after_source_edit = digest();
    write(&tmp.path().join("a/src/lib.rs"), "");
    assert_eq!(
        after_source_edit,
        digest(),
        "Rust and Markdown are not assets"
    );
}
