//! Daemon-side staging-GC fixtures (#1882 unit 10): real directories under a
//! temp cache root, swept through the same entry point the daemon calls.
use super::*;
use std::time::{Duration, SystemTime};

/// Create `cache/prebuilt/fastled/3.3.12/esp32` and return it.
fn prebuilt_tree(cache: &NormalizedPath) -> NormalizedPath {
    let tree = cache
        .join("prebuilt")
        .join("fastled")
        .join("3.3.12")
        .join("esp32");
    std::fs::create_dir_all(tree.as_path()).unwrap();
    tree
}

/// Create one `.<sha16>.<attempt>.staging` sibling holding a partial file.
fn staging_sibling(parent: &NormalizedPath, attempt: usize) -> NormalizedPath {
    let path = parent.join(format!(".0123456789abcdef.{attempt}.staging"));
    std::fs::create_dir_all(path.as_path()).unwrap();
    std::fs::write(path.join("partial.a").as_path(), vec![b'x'; 16]).unwrap();
    path
}

#[tokio::test]
async fn the_walk_reaches_a_staging_sibling_under_the_prebuilt_root() {
    let cache = tempfile::tempdir().unwrap();
    let root = NormalizedPath::new(cache.path());
    let tree = prebuilt_tree(&root);
    let live = staging_sibling(&tree, 0);
    let report = sweep_once(root.as_path()).await.unwrap();
    // A fresh attempt is *kept* and counted: the count is what proves the
    // walk reached the leaf, not just the root.
    assert_eq!(report.kept_live, 1);
    assert_eq!(report.removed, 0);
    assert!(live.as_path().is_dir());
}

#[tokio::test]
async fn a_staging_directory_outside_the_prebuilt_root_is_not_swept() {
    let cache = tempfile::tempdir().unwrap();
    let root = NormalizedPath::new(cache.path());
    let foreign = root.join("archives").join("tree");
    std::fs::create_dir_all(foreign.as_path()).unwrap();
    let kept = staging_sibling(&foreign, 3);
    let report = sweep_once(root.as_path()).await.unwrap();
    assert_eq!(report, SweepReport::default());
    assert!(kept.as_path().is_dir());
}

#[tokio::test]
async fn a_cache_without_a_prebuilt_root_is_a_quiet_sweep() {
    let cache = tempfile::tempdir().unwrap();
    let root = NormalizedPath::new(cache.path());
    let report = sweep_once(root.as_path()).await.unwrap();
    assert_eq!(report, SweepReport::default());
}

/// A completed tree with a marker stamped `age` in the past.
fn completed_tree(
    cache: &NormalizedPath,
    leaf: &str,
    size: usize,
    age: Duration,
) -> NormalizedPath {
    let tree = prebuilt_tree(cache).join(leaf);
    std::fs::create_dir_all(tree.as_path()).unwrap();
    std::fs::write(tree.join("libesp32.a").as_path(), vec![b'x'; size]).unwrap();
    let marker = tree.join(fbuild_packages::prebuilt::MARKER);
    std::fs::write(
        marker.as_path(),
        b"{\"sha256\":\"b\",\"size_bytes\":1,\"entries\":1}",
    )
    .unwrap();
    std::fs::File::options()
        .write(true)
        .open(marker.as_path())
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(SystemTime::now() - age))
        .unwrap();
    tree
}

#[tokio::test]
async fn the_lru_prunes_a_tree_older_than_the_grace_period() {
    let cache = tempfile::tempdir().unwrap();
    let root = NormalizedPath::new(cache.path());
    let stale = completed_tree(
        &root,
        "fp-aaaaaaaaaaaaaaaa",
        512,
        MIN_AGE + Duration::from_secs(60),
    );
    let fresh = completed_tree(&root, "fp-bbbbbbbbbbbbbbbb", 512, Duration::ZERO);
    // A zero budget means "keep nothing eligible": the stale tree goes, the
    // fresh one is protected by the grace period.
    let report = prune_once(root.as_path(), 0).await.unwrap();
    assert_eq!(report.removed, 1);
    assert_eq!(report.kept_live, 1);
    assert_eq!(report.budget_bytes, 0);
    assert!(!stale.as_path().exists());
    assert!(fresh.as_path().is_dir());
}

#[tokio::test]
async fn the_lru_leaves_a_generous_budget_alone() {
    let cache = tempfile::tempdir().unwrap();
    let root = NormalizedPath::new(cache.path());
    let tree = completed_tree(&root, "fp-aaaaaaaaaaaaaaaa", 64, MIN_AGE * 10);
    let report = prune_once(root.as_path(), 1024 * 1024).await.unwrap();
    assert_eq!(report.removed, 0);
    assert_eq!(report.trees, 1);
    assert!(tree.as_path().is_dir());
}

#[test]
fn byte_counts_are_readable() {
    assert_eq!(format_bytes(0), "0 B");
    assert_eq!(format_bytes(2048), "2.0 KiB");
    assert_eq!(format_bytes(5 * 1024 * 1024), "5.0 MiB");
}
