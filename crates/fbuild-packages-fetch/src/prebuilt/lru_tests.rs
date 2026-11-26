//! Disk-cache LRU fixtures (#1882): real trees, real markers, a synthetic
//! clock so the grace period and the recency order are both testable.
use super::*;
use crate::disk_cache::paths::prebuilt_root;
use std::io::Write;

/// The grace period the eviction rule uses, as the daemon passes it.
const MIN_AGE: Duration = crate::prebuilt::gc::MIN_AGE;

/// A marker body of the shape extraction writes; the LRU never parses it.
const MARKER_BODY: &str = r#"{"sha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","size_bytes":8,"entries":1}"#;

fn cache() -> (tempfile::TempDir, NormalizedPath) {
    let dir = tempfile::tempdir().unwrap();
    let root = NormalizedPath::new(prebuilt_root(dir.path()));
    std::fs::create_dir_all(root.as_path()).unwrap();
    (dir, root)
}

/// A tree at `<root>/<framework>/<version>/<arch>/<leaf>`, holding one file of
/// `size` bytes and a marker stamped at `used` seconds before `now`.
fn tree(
    root: &NormalizedPath,
    leaf: &str,
    size: usize,
    used_before: Duration,
    now: SystemTime,
) -> NormalizedPath {
    let path = root.join("arduino-esp32/3.1.1/esp32").join(leaf);
    std::fs::create_dir_all(path.as_path()).unwrap();
    let mut stream = std::fs::File::create(path.join("libesp32.a").as_path()).unwrap();
    stream.write_all(&vec![b'x'; size]).unwrap();
    let marker = path.join(MARKER);
    std::fs::write(marker.as_path(), MARKER_BODY).unwrap();
    let when = now - used_before;
    std::fs::File::options()
        .write(true)
        .open(marker.as_path())
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(when))
        .unwrap();
    path
}

fn clock() -> SystemTime {
    SystemTime::now()
}

#[test]
fn only_a_directory_holding_a_regular_marker_is_a_candidate() {
    let (_temp, root) = cache();
    let marked = tree(
        &root,
        "fp-aaaaaaaaaaaaaaaa",
        16,
        Duration::from_secs(7200),
        clock(),
    );
    // A staging sibling, a partial destination and a loose file are all the
    // sweep's business, never the LRU's.
    std::fs::create_dir_all(
        root.join("arduino-esp32/3.1.1/esp32/.bbbbbbbbbbbbbbbb.0.staging")
            .as_path(),
    )
    .unwrap();
    std::fs::create_dir_all(
        root.join("arduino-esp32/3.1.1/esp32/fp-cccccccccccccccc")
            .as_path(),
    )
    .unwrap();
    std::fs::write(root.join("arduino-esp32/3.1.1/stray.txt").as_path(), b"x").unwrap();

    let found = trees(_temp.path()).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].root, marked);
    assert_eq!(found[0].bytes, 16 + MARKER_BODY.len() as u64);
}

#[test]
fn the_prebuilt_root_itself_is_never_a_candidate() {
    let (temp, root) = cache();
    std::fs::write(root.join(MARKER).as_path(), MARKER_BODY).unwrap();
    assert!(trees(temp.path()).unwrap().is_empty());
    let report = prune(temp.path(), 0, MIN_AGE).unwrap();
    assert_eq!(report.removed, 0);
    assert!(
        root.join(MARKER).as_path().is_file(),
        "the root was removed"
    );
}

#[test]
fn a_marker_that_is_not_a_regular_file_is_not_a_candidate() {
    let (temp, root) = cache();
    let path = root.join("arduino-esp32/3.1.1/esp32/fp-aaaaaaaaaaaaaaaa");
    std::fs::create_dir_all(path.as_path()).unwrap();
    // A directory named like the marker: presence alone must never claim a tree.
    std::fs::create_dir_all(path.join(MARKER).as_path()).unwrap();
    assert!(trees(temp.path()).unwrap().is_empty());
}

#[test]
fn a_symlinked_tree_is_never_entered_or_removed() {
    let (temp, root) = cache();
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("keep.txt"), b"keep").unwrap();
    std::fs::write(outside.join(MARKER), MARKER_BODY).unwrap();
    let parent = root.join("arduino-esp32/3.1.1/esp32");
    std::fs::create_dir_all(parent.as_path()).unwrap();
    let link = parent.join("fp-dddddddddddddddd");
    fbuild_core::platform::fs::symlink_dir(&outside, link.as_path()).unwrap();

    let report = prune(temp.path(), 0, MIN_AGE).unwrap();
    assert_eq!(report.trees, 0);
    assert_eq!(report.removed, 0);
    assert!(
        outside.join("keep.txt").is_file(),
        "the link target was touched"
    );
}

#[test]
fn the_oldest_trees_are_evicted_until_the_budget_fits() {
    let (temp, root) = cache();
    let now = clock();
    let oldest = tree(
        &root,
        "fp-aaaaaaaaaaaaaaaa",
        1000,
        Duration::from_secs(7200),
        now,
    );
    let middle = tree(
        &root,
        "fp-bbbbbbbbbbbbbbbb",
        2000,
        Duration::from_secs(3600),
        now,
    );
    let newest = tree(
        &root,
        "fp-cccccccccccccccc",
        4000,
        Duration::from_secs(1800),
        now,
    );
    // 7000 B of trees plus three markers; a 5000 B budget only fits the newest.
    let report = prune(temp.path(), 5000, MIN_AGE).unwrap();
    assert_eq!(report.trees, 3);
    assert_eq!(report.removed, 2);
    assert_eq!(report.kept_live, 0);
    assert_eq!(report.freed_bytes, 3000 + 2 * MARKER_BODY.len() as u64);
    assert_eq!(
        report.remaining_bytes,
        report.scanned_bytes - report.freed_bytes
    );
    assert!(report.remaining_bytes <= 5000);
    assert!(!oldest.as_path().exists());
    assert!(!middle.as_path().exists());
    assert!(newest.join("libesp32.a").as_path().is_file());
}

#[test]
fn a_tree_inside_the_grace_period_is_kept_and_counted() {
    let (temp, root) = cache();
    let now = clock();
    tree(
        &root,
        "fp-aaaaaaaaaaaaaaaa",
        32,
        Duration::from_secs(1),
        now,
    );
    tree(
        &root,
        "fp-bbbbbbbbbbbbbbbb",
        32,
        Duration::from_secs(60),
        now,
    );
    let report = prune(temp.path(), 0, MIN_AGE).unwrap();
    assert_eq!(report.removed, 0);
    assert_eq!(report.kept_live, 2);
    assert_eq!(report.remaining_bytes, report.scanned_bytes);
}

#[test]
fn a_reuse_moves_a_tree_out_of_eviction_order() {
    let (temp, root) = cache();
    let now = clock();
    let reused = tree(
        &root,
        "fp-aaaaaaaaaaaaaaaa",
        64,
        Duration::from_secs(7200),
        now,
    );
    let idle = tree(
        &root,
        "fp-bbbbbbbbbbbbbbbb",
        64,
        Duration::from_secs(7200),
        now,
    );
    touch(reused.as_path(), now - Duration::from_secs(60)).unwrap();
    let report = prune(temp.path(), 0, MIN_AGE).unwrap();
    assert_eq!(report.removed, 1);
    assert_eq!(report.kept_live, 1);
    assert!(
        reused.join("libesp32.a").as_path().is_file(),
        "the reused tree was evicted"
    );
    assert!(!idle.as_path().exists(), "the untouched tree survived");
}

#[test]
fn touching_an_absent_tree_is_not_an_error() {
    let (_temp, root) = cache();
    let absent = root.join("arduino-esp32/3.1.1/esp32/fp-eeeeeeeeeeeeeeee");
    assert!(touch(absent.as_path(), clock()).is_ok());
    assert!(!absent.as_path().exists(), "touch must never create a tree");
}

#[test]
fn prune_removes_at_most_max_removals() {
    let (temp, root) = cache();
    let now = clock();
    for index in 0..MAX_REMOVALS + 4 {
        let leaf = format!("fp-{index:016x}");
        tree(&root, &leaf, 8, Duration::from_secs(7200), now);
    }
    let report = prune(temp.path(), 0, MIN_AGE).unwrap();
    assert_eq!(report.removed, MAX_REMOVALS);
    assert_eq!(report.trees, MAX_REMOVALS + 4);
    assert!(report.remaining_bytes > 0);
}

#[test]
fn an_absent_prebuilt_root_is_an_empty_prune() {
    let temp = tempfile::tempdir().unwrap();
    let report = prune(temp.path(), 0, MIN_AGE).unwrap();
    assert_eq!(report, EvictReport::default());
}

#[test]
fn the_budget_is_opt_in_and_never_guessed() {
    assert_eq!(budget_from(None), None, "unset must disable the LRU");
    assert_eq!(budget_from(Some("")), None);
    assert_eq!(budget_from(Some("   ")), None);
    assert_eq!(
        budget_from(Some("two gigabytes")),
        None,
        "a typo disables, never guesses"
    );
    assert_eq!(budget_from(Some("2G")), Some(2 * 1024 * 1024 * 1024));
    assert_eq!(budget_from(Some("512M")), Some(512 * 1024 * 1024));
    assert_eq!(budget_from(Some("0")), Some(0), "zero is a real budget");
}
