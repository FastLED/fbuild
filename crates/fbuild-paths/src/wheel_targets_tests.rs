//! Tests for the wheel-build target store survey (FastLED/fbuild#2062).
//!
//! Every case builds its own store under a temp dir, so nothing depends on
//! this host's real `~/.fbuild/cargo-target/wheel-build`.

use std::fs;
use std::time::{Duration, SystemTime};

use fbuild_core::path::NormalizedPath;

use crate::FBUILD_DIR_NAME;

use super::{
    DEFAULT_IDLE_DAYS, ReclaimVerdict, reclaim, recorded_owner, scan_tree, source_stamp_name,
    store_root, survey, survey_with_budget,
};

const DAY: Duration = Duration::from_secs(86_400);

/// A fresh store directory, plus its normalized handle.
fn store() -> (tempfile::TempDir, NormalizedPath) {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = NormalizedPath::new(dir.path()).join("wheel-build");
    fs::create_dir_all(store.as_path()).expect("store dir");
    (dir, store)
}

/// One target dir holding a single file of `bytes` bytes.
fn target(store: &NormalizedPath, name: &str, bytes: usize) -> NormalizedPath {
    let target = store.join(name);
    fs::create_dir_all(target.as_path()).expect("target dir");
    fs::write(target.join("artifact.rlib"), vec![b'x'; bytes]).expect("artifact");
    target
}

/// Bytes the stamp file itself adds to a target: `owner` plus its newline.
fn stamp_bytes(owner: &str) -> u64 {
    owner.len() as u64 + 1
}

/// Stamp `target` with `owner` as the checkout that owns it.
fn stamp(target: &NormalizedPath, owner: &str) {
    fs::write(target.join(source_stamp_name()), format!("{owner}\n")).expect("stamp");
}

#[test]
fn store_root_keeps_the_fbuild_segment_before_cargo_target() {
    let store = store_root().expect("this host has a home directory");
    let home = fbuild_core::platform::host::home_dir().expect("this host has a home directory");
    // The wheel build keys the store under the home directory's fbuild segment,
    // so dropping that segment would point at a directory nothing writes.
    let relative = store
        .relative_to(&home)
        .expect("the store lives under the home directory");
    assert_eq!(
        relative.display_slash(),
        format!("{FBUILD_DIR_NAME}/cargo-target/wheel-build"),
        "store root is <home>/{FBUILD_DIR_NAME}/cargo-target/wheel-build"
    );
}

#[test]
fn a_stamped_target_whose_owner_is_gone_is_dead() {
    let (_dir, store) = store();
    let target = target(&store, "fbuild-0123456789ab", 64);
    stamp(&target, "/checkouts/fbuild-wt-deleted");

    let measured = survey(store.as_path(), DEFAULT_IDLE_DAYS, SystemTime::now());
    assert!(measured.present(), "the store exists");
    let entry = &measured.targets()[0];
    assert_eq!(entry.name(), "fbuild-0123456789ab");
    assert_eq!(entry.owner(), Some("/checkouts/fbuild-wt-deleted"));
    match entry.verdict() {
        Some(ReclaimVerdict::OwnerGone { owner }) => {
            assert_eq!(owner, "/checkouts/fbuild-wt-deleted");
        }
        other => panic!("owner gone must be reclaimable, got {other:?}"),
    }
    let owner = "/checkouts/fbuild-wt-deleted";
    assert_eq!(measured.reclaimable_bytes(), 64 + stamp_bytes(owner));
    assert_eq!(measured.total_bytes(), 64 + stamp_bytes(owner));
}

#[test]
fn a_stamped_target_whose_owner_still_exists_is_kept() {
    let (root, store) = store();
    let owner = NormalizedPath::new(root.path()).join("live-worktree");
    fs::create_dir_all(owner.as_path()).expect("live worktree");
    let target = target(&store, "fbuild-aaaaaaaaaaaa", 64);
    stamp(&target, &owner.display_slash());

    let measured = survey(store.as_path(), DEFAULT_IDLE_DAYS, SystemTime::now());
    assert_eq!(measured.targets()[0].verdict(), None);
    assert_eq!(measured.reclaimable_bytes(), 0);
    assert_eq!(
        measured.total_bytes(),
        64 + stamp_bytes(&owner.display_slash())
    );
}

#[test]
fn an_unstamped_target_older_than_the_gate_is_dead() {
    let (_dir, store) = store();
    target(&store, "fbuild-bbbbbbbbbbbb", 32);

    let later = SystemTime::now() + 15 * DAY;
    let measured = survey(store.as_path(), DEFAULT_IDLE_DAYS, later);
    assert_eq!(
        measured.targets()[0].verdict(),
        Some(&ReclaimVerdict::IdleWithoutStamp)
    );
    assert_eq!(measured.reclaimable_bytes(), 32);
}

#[test]
fn an_unstamped_target_within_the_gate_is_kept() {
    let (_dir, store) = store();
    target(&store, "fbuild-cccccccccccc", 32);

    let measured = survey(store.as_path(), DEFAULT_IDLE_DAYS, SystemTime::now());
    assert_eq!(
        measured.targets()[0].verdict(),
        None,
        "a target just written is not idle"
    );
}

#[test]
fn a_blank_stamp_is_treated_as_unstamped() {
    let (_dir, store) = store();
    let target = target(&store, "fbuild-dddddddddddd", 8);
    fs::write(target.join(source_stamp_name()), "   \n").expect("blank stamp");

    let later = SystemTime::now() + 15 * DAY;
    let measured = survey(store.as_path(), DEFAULT_IDLE_DAYS, later);
    assert_eq!(
        measured.targets()[0].verdict(),
        Some(&ReclaimVerdict::IdleWithoutStamp),
        "a stamp without a path is not evidence of a live owner"
    );
}

#[test]
fn a_missing_store_is_reported_absent_rather_than_failing() {
    let dir = tempfile::tempdir().expect("temp dir");
    let missing = NormalizedPath::new(dir.path()).join("never-built");

    let measured = survey(missing.as_path(), DEFAULT_IDLE_DAYS, SystemTime::now());
    assert!(!measured.present(), "a store that does not exist is absent");
    assert!(measured.targets().is_empty());
    assert_eq!(measured.total_bytes(), 0);
    assert_eq!(measured.reclaimable_bytes(), 0);
}

#[test]
fn the_survey_lists_the_largest_target_first() {
    let (_dir, store) = store();
    target(&store, "fbuild-small", 8);
    target(&store, "fbuild-large", 512);

    let measured = survey(store.as_path(), DEFAULT_IDLE_DAYS, SystemTime::now());
    let names: Vec<&str> = measured.targets().iter().map(|t| t.name()).collect();
    assert_eq!(names, vec!["fbuild-large", "fbuild-small"]);
}

#[test]
fn reclaim_removes_only_the_dead_targets() {
    let (root, store) = store();
    let owner = NormalizedPath::new(root.path()).join("live-worktree");
    fs::create_dir_all(owner.as_path()).expect("live worktree");

    let live = target(&store, "fbuild-live0000000", 16);
    stamp(&live, &owner.display_slash());
    let dead = target(&store, "fbuild-dead0000000", 128);
    stamp(&dead, "/checkouts/gone");

    let reclaimed = reclaim(store.as_path(), DEFAULT_IDLE_DAYS, SystemTime::now());
    assert_eq!(reclaimed.len(), 1, "one dead target: {reclaimed:?}");
    assert_eq!(reclaimed[0].name(), "fbuild-dead0000000");
    assert_eq!(reclaimed[0].reason(), "owner gone: /checkouts/gone");
    assert_eq!(
        reclaimed[0].bytes_freed(),
        128 + stamp_bytes("/checkouts/gone")
    );
    assert!(live.as_path().exists(), "the live target survives");
    assert!(!dead.as_path().exists(), "the dead target is gone");
}

#[test]
fn reclaim_on_an_absent_store_is_a_no_op() {
    let dir = tempfile::tempdir().expect("temp dir");
    let missing = NormalizedPath::new(dir.path()).join("never-built");
    assert!(reclaim(missing.as_path(), DEFAULT_IDLE_DAYS, SystemTime::now()).is_empty());
    assert!(
        !missing.as_path().exists(),
        "reclaim never creates the store"
    );
}

#[test]
fn a_tree_over_the_scan_budget_is_kept() {
    let (_dir, store) = store();
    let target = target(&store, "fbuild-toobig00000", 8);
    for index in 0..4 {
        fs::write(target.join(format!("extra-{index}.rlib")), b"x").expect("extra");
    }
    assert!(
        scan_tree(target.as_path(), 100).is_some(),
        "a small tree is measured"
    );
    assert!(
        scan_tree(target.as_path(), 2).is_none(),
        "the walk reports over-budget instead of guessing"
    );

    let later = SystemTime::now() + 30 * DAY;
    let measured = survey_with_budget(store.as_path(), DEFAULT_IDLE_DAYS, later, 2);
    assert_eq!(
        measured.targets()[0].verdict(),
        None,
        "an unmeasurable tree is never reclaimed"
    );
}

#[test]
fn a_recorded_owner_is_read_back_trimmed() {
    let (_dir, store) = store();
    let target = target(&store, "fbuild-eeeeeeeeeeee", 4);
    stamp(&target, "/checkouts/fbuild");
    assert_eq!(
        recorded_owner(target.as_path()).as_deref(),
        Some("/checkouts/fbuild")
    );
}

#[test]
fn a_negative_idle_gate_falls_back_to_the_default() {
    let (_dir, store) = store();
    target(&store, "fbuild-ffffffffffff", 4);
    let measured = survey(store.as_path(), -1.0, SystemTime::now());
    assert_eq!(measured.targets().len(), 1, "the entry is still measured");
    assert_eq!(
        measured.targets()[0].verdict(),
        None,
        "a fresh target is kept even with a nonsense gate"
    );
}
