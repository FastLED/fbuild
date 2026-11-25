//! Staging/partial GC fixtures (#1882): real directories, a synthetic clock.
use super::*;
use std::io::Write;

/// A digest prefix of the shape extraction produces.
const DIGEST: &str = "0123456789abcdef";

fn layout() -> (tempfile::TempDir, NormalizedPath) {
    let dir = tempfile::tempdir().unwrap();
    let root = NormalizedPath::new(dir.path().join("trees"));
    std::fs::create_dir_all(root.as_path()).unwrap();
    (dir, root)
}

/// Create a directory named `name` under `parent`, holding one file of `size`.
fn attempt(parent: &NormalizedPath, name: &str, size: usize) -> NormalizedPath {
    let path = parent.join(name);
    std::fs::create_dir_all(path.as_path()).unwrap();
    let file = path.join("libexample.a");
    let mut stream = std::fs::File::create(file.as_path()).unwrap();
    stream.write_all(&vec![b'x'; size]).unwrap();
    path
}

/// A clock far enough ahead to age every fixture past any grace period.
fn later() -> SystemTime {
    SystemTime::now() + Duration::from_secs(60 * 60 * 24 * 365)
}

#[test]
fn a_stale_staging_directory_is_removed_and_counted() {
    let (_temp, root) = layout();
    let abandoned = attempt(&root, ".0123456789abcdef.0.staging", 1000);
    attempt(&root, ".fedcba9876543210.3.staging", 24);
    let report = sweep_staging_at(root.as_path(), MIN_AGE, later()).unwrap();
    assert_eq!(report.removed, 2);
    assert_eq!(report.freed_bytes, 1024);
    assert_eq!(report.kept_live, 0);
    assert_eq!(report.kept_marked, 0);
    assert!(!abandoned.as_path().exists());
    assert!(root.as_path().is_dir(), "the parent is never removed");
}

#[test]
fn a_fresh_staging_directory_is_kept_for_a_live_extraction() {
    let (_temp, root) = layout();
    let live = attempt(&root, ".0123456789abcdef.7.staging", 16);
    let report = sweep_staging_at(root.as_path(), MIN_AGE, SystemTime::now()).unwrap();
    assert_eq!(
        report,
        SweepReport {
            removed: 0,
            freed_bytes: 0,
            kept_live: 1,
            kept_marked: 0
        }
    );
    assert!(live.join("libexample.a").as_path().is_file());
}

#[test]
fn a_completed_tree_and_foreign_names_are_never_touched() {
    let (_temp, root) = layout();
    let tree = attempt(&root, "fp-0123456789abcdef", 32);
    std::fs::write(tree.join(MARKER).as_path(), b"{}").unwrap();
    // One character off in each direction: prefix, case, width, attempt, dot.
    let shapes = [
        ".0123456789abcdef.0.staging.bak",
        "0123456789abcdef.0.staging",
        ".0123456789ABCDEF.0.staging",
        ".0123456789abcde.0.staging",
        ".0123456789abcdef.0.staging2",
        ".0123456789abcdef.z.staging",
        ".0123456789abcdef..staging",
    ];
    for shape in shapes {
        std::fs::create_dir_all(root.join(shape).as_path()).unwrap();
    }
    let report = sweep_staging_at(root.as_path(), MIN_AGE, later()).unwrap();
    assert_eq!(report.removed, 0);
    assert_eq!(report.freed_bytes, 0);
    assert!(tree.join(MARKER).as_path().is_file());
    for shape in shapes {
        assert!(root.join(shape).as_path().is_dir(), "{shape} was removed");
    }
}

#[test]
fn a_staging_name_that_is_a_file_is_left_alone() {
    let (_temp, root) = layout();
    let file = root.join(".0123456789abcdef.1.staging");
    std::fs::write(file.as_path(), b"a partial download, not a tree").unwrap();
    let report = sweep_staging_at(root.as_path(), MIN_AGE, later()).unwrap();
    assert_eq!(report.removed, 0);
    assert!(file.as_path().is_file());
}

#[test]
fn a_staging_directory_holding_a_marker_is_kept_and_reported() {
    let (_temp, root) = layout();
    let verified = attempt(&root, ".0123456789abcdef.1.staging", 64);
    std::fs::write(verified.join(MARKER).as_path(), b"{}").unwrap();
    let report = sweep_staging_at(root.as_path(), MIN_AGE, later()).unwrap();
    assert_eq!(report.removed, 0);
    assert_eq!(report.kept_marked, 1);
    assert!(verified.join(MARKER).as_path().is_file());
}

#[test]
fn a_symlink_named_like_staging_is_not_followed_or_unlinked() {
    let (_temp, root) = layout();
    let real = attempt(&root, "elsewhere", 40);
    let link = root.join(".0123456789abcdef.2.staging");
    fbuild_core::platform::fs::symlink_dir(real.as_path(), link.as_path()).unwrap();
    let report = sweep_staging_at(root.as_path(), MIN_AGE, later()).unwrap();
    assert_eq!(report.removed, 0);
    assert!(real.join("libexample.a").as_path().is_file());
    // The link itself is left in place: a sweep never unlinks a candidate.
    assert!(std::fs::symlink_metadata(link.as_path()).is_ok());
}

#[test]
fn a_missing_parent_is_an_empty_sweep_not_an_error() {
    let (temp, _root) = layout();
    let absent = NormalizedPath::new(temp.path().join("never-created"));
    let report = sweep_staging_at(absent.as_path(), MIN_AGE, later()).unwrap();
    assert_eq!(report, SweepReport::default());
}

#[test]
fn one_sweep_is_bounded_by_the_removal_ceiling() {
    let (_temp, root) = layout();
    let total = MAX_REMOVALS + 3;
    for index in 0..total {
        attempt(&root, &format!(".0123456789abcdef.{index}.staging"), 1);
    }
    let report = sweep_staging_at(root.as_path(), MIN_AGE, later()).unwrap();
    assert_eq!(report.removed, MAX_REMOVALS);
    let left = std::fs::read_dir(root.as_path()).unwrap().count();
    assert_eq!(left, total - MAX_REMOVALS);
}

#[tokio::test]
async fn the_async_sweep_removes_an_abandoned_attempt() {
    let (_temp, root) = layout();
    let abandoned = attempt(&root, &format!(".{DIGEST}.0.staging"), 8);
    let report = sweep_staging_async(root.clone(), Duration::ZERO)
        .await
        .unwrap();
    assert_eq!(report.removed, 1);
    assert_eq!(report.freed_bytes, 8);
    assert!(!abandoned.as_path().exists());
}

#[test]
fn the_staging_name_shape_is_exact() {
    assert!(is_staging_name(&format!(".{DIGEST}.0.staging")));
    assert!(is_staging_name(&format!(".{DIGEST}.129.staging")));
    assert!(!is_staging_name(&format!("{DIGEST}.0.staging")));
    assert!(!is_staging_name(&format!(".{DIGEST}.0")));
    assert!(!is_staging_name(&format!(".{DIGEST}.staging")));
    // One digit short: a foreign dotfile, not the shape extraction writes.
    assert!(!is_staging_name(&format!(".{}.0.staging", &DIGEST[..15])));
    // One digit long, and not lowercase hex.
    assert!(!is_staging_name(&format!(".{DIGEST}0.0.staging")));
    assert!(!is_staging_name(".0123456789abcdeF.0.staging"));
}
