use super::component_id::{
    ComponentIdentity, SCHEMA, STAMP_FILE, content_id, read_stamp, write_stamp,
};
use std::path::Path;

fn staged(files: &[(&str, &[u8])]) -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().unwrap();
    for (relative, bytes) in files {
        let path = dir.path().join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, bytes).unwrap();
    }
    dir
}

fn core(files: &[(&str, &[u8])]) -> String {
    content_id(
        staged(files).path(),
        &["tools/esp32-arduino-libs", "tools/.libs-archives"],
    )
    .unwrap()
}

#[test]
fn identical_trees_share_an_id() {
    let files: &[(&str, &[u8])] = &[("cores/esp32/Arduino.h", b"#pragma once"), ("a.txt", b"a")];
    assert_eq!(core(files), core(files));
}

#[test]
fn a_byte_change_changes_the_id() {
    let before = core(&[("cores/esp32/Arduino.h", b"#pragma once")]);
    let after = core(&[("cores/esp32/Arduino.h", b"#pragma oncd")]);
    assert_ne!(before, after);
}

#[test]
fn a_path_rename_changes_the_id() {
    let before = core(&[("cores/esp32/Arduino.h", b"x")]);
    let after = core(&[("cores/esp32/Arduino.hpp", b"x")]);
    assert_ne!(before, after);
}

#[test]
fn an_added_file_changes_the_id() {
    let before = core(&[("a.txt", b"a")]);
    let after = core(&[("a.txt", b"a"), ("b.txt", b"b")]);
    assert_ne!(before, after);
}

#[test]
fn the_stamp_file_is_not_part_of_the_id() {
    let dir = staged(&[("a.txt", b"a")]);
    let excluded: &[&str] = &["tools/esp32-arduino-libs"];
    let before = content_id(dir.path(), excluded).unwrap();
    write_stamp(
        dir.path(),
        &ComponentIdentity::new("core", "1.0", before.clone(), 0),
    )
    .unwrap();
    assert_eq!(content_id(dir.path(), excluded).unwrap(), before);
}

#[test]
fn the_install_sentinel_is_not_part_of_the_id() {
    let dir = staged(&[("a.txt", b"a")]);
    let before = content_id(dir.path(), &[]).unwrap();
    std::fs::write(dir.path().join(".install_complete"), b"{}").unwrap();
    assert_eq!(content_id(dir.path(), &[]).unwrap(), before);
}

#[test]
fn an_excluded_subtree_is_invisible() {
    let excluded: &[&str] = &["tools/esp32-arduino-libs", "tools/.libs-archives"];
    let plain = staged(&[("cores/esp32/Arduino.h", b"x")]);
    // The SDK merge writes into tools/ after the core install commits, so the
    // core identity must not move when it does.
    let with_sdk = staged(&[
        ("cores/esp32/Arduino.h", b"x"),
        ("tools/esp32-arduino-libs/esp32s3/lib/libesp32.a", b"sdk"),
        ("tools/.libs-archives/x/y.a", b"arch"),
    ]);
    assert_eq!(
        content_id(plain.path(), excluded).unwrap(),
        content_id(with_sdk.path(), excluded).unwrap()
    );
}

#[test]
fn an_exclusion_only_prunes_its_own_subtree() {
    let excluded: &[&str] = &["tools/esp32-arduino-libs"];
    // `tools/esp32-arduino-libs-notes/` merely shares the prefix.
    let with_sibling = staged(&[
        ("a.txt", b"a"),
        ("tools/esp32-arduino-libs-notes/readme", b"n"),
    ]);
    let without = staged(&[("a.txt", b"a")]);
    assert_ne!(
        content_id(with_sibling.path(), excluded).unwrap(),
        content_id(without.path(), excluded).unwrap()
    );
}

#[test]
fn a_symlink_target_changes_the_id() {
    let one = staged(&[("a.txt", b"a")]);
    let two = staged(&[("b.txt", b"a")]);
    let link = |dir: &Path, target: &str| {
        let path = dir.join("link");
        fbuild_core::platform::fs::symlink_file(Path::new(target), &path).unwrap();
    };
    link(one.path(), "a.txt");
    link(two.path(), "b.txt");
    assert_ne!(
        content_id(one.path(), &[]).unwrap(),
        content_id(two.path(), &[]).unwrap()
    );
}

#[test]
fn an_exec_bit_change_changes_the_id() {
    if !fbuild_core::platform::host::current().is_unix() {
        return;
    }
    let dir = staged(&[("tool.com", b"binary")]);
    let before = content_id(dir.path(), &[]).unwrap();
    fbuild_core::platform::fs::set_executable(&dir.path().join("tool.com")).unwrap();
    assert_ne!(content_id(dir.path(), &[]).unwrap(), before);
}

#[test]
fn a_missing_root_is_an_error_not_an_empty_id() {
    let dir = tempfile::TempDir::new().unwrap();
    let missing = dir.path().join("nope");
    assert!(content_id(&missing, &[]).is_err());
}

#[test]
fn a_stamp_round_trips() {
    let dir = staged(&[("a.txt", b"a")]);
    let identity = ComponentIdentity::new("core", "3.3.11", "abc123".to_string(), 0);
    write_stamp(dir.path(), &identity).unwrap();
    assert!(dir.path().join(STAMP_FILE).is_file());
    assert_eq!(read_stamp(dir.path()), Some(identity));
}

#[test]
fn an_absent_stamp_is_no_identity() {
    let dir = staged(&[("a.txt", b"a")]);
    assert_eq!(read_stamp(dir.path()), None);
}

#[test]
fn a_stamp_from_another_schema_is_no_identity() {
    let dir = staged(&[("a.txt", b"a")]);
    let mut identity = ComponentIdentity::new("core", "3.3.11", "abc".to_string(), 0);
    identity.schema = SCHEMA + 1;
    std::fs::write(
        dir.path().join(STAMP_FILE),
        serde_json::to_vec(&identity).unwrap(),
    )
    .unwrap();
    assert_eq!(read_stamp(dir.path()), None);
}

#[test]
fn an_unreadable_stamp_is_no_identity() {
    let dir = staged(&[("a.txt", b"a")]);
    std::fs::write(dir.path().join(STAMP_FILE), b"{not json").unwrap();
    assert_eq!(read_stamp(dir.path()), None);
}
