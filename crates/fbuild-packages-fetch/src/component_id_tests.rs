use super::component_id::{
    ComponentIdentity, SCHEMA, component_term, content_id, entries_survived, has_stamp,
    is_stamp_file, read_stamp, remove_stamp, stamp_file_name, write_stamp,
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
    assert!(dir.path().join(stamp_file_name("core")).is_file());
    assert_eq!(read_stamp(dir.path(), "core"), Some(identity));
}

#[test]
fn two_components_share_one_directory_without_clobbering_each_other() {
    // The SDK variant and the MCU skeleton of the same chip both install into
    // `tools/esp32-arduino-libs/<mcu>/`; one stamp file per directory would make
    // the second component's identity overwrite the first's.
    let dir = staged(&[("lib/libfreertos.a", b"freertos")]);
    let sdk = ComponentIdentity::new("sdk-esp32c2", "3.3.12", "sdk-id".to_string(), 1);
    let mcu = ComponentIdentity::new("mcu-esp32c2", "c2_skeleton", "mcu-id".to_string(), 1);
    write_stamp(dir.path(), &sdk).unwrap();
    write_stamp(dir.path(), &mcu).unwrap();

    assert_eq!(read_stamp(dir.path(), "sdk-esp32c2"), Some(sdk));
    assert_eq!(read_stamp(dir.path(), "mcu-esp32c2"), Some(mcu));
    assert!(has_stamp(dir.path()));

    // Withdrawing one component leaves the other's identity in place.
    remove_stamp(dir.path(), "mcu-esp32c2").unwrap();
    assert!(read_stamp(dir.path(), "sdk-esp32c2").is_some());
    assert!(read_stamp(dir.path(), "mcu-esp32c2").is_none());
}

#[test]
fn a_stamp_filed_under_another_component_is_no_identity() {
    let dir = staged(&[("a.txt", b"a")]);
    write_stamp(
        dir.path(),
        &ComponentIdentity::new("sdk-esp32c2", "1", "id".to_string(), 0),
    )
    .unwrap();
    assert!(read_stamp(dir.path(), "mcu-esp32c2").is_none());
    assert!(read_stamp(dir.path(), "sdk-esp32c2").is_some());
}

#[test]
fn a_component_name_cannot_escape_its_stamp_file() {
    let dir = staged(&[("a.txt", b"a")]);
    let identity = ComponentIdentity::new("../escape", "1", "id".to_string(), 0);
    assert!(write_stamp(dir.path(), &identity).is_err());
}

#[test]
fn a_stamp_file_name_is_recognised_only_at_a_component_root() {
    assert!(is_stamp_file(&stamp_file_name("core")));
    assert!(is_stamp_file(&stamp_file_name("mcu-esp32c2")));
    assert!(!is_stamp_file("component-identity.json"));
    assert!(!is_stamp_file("nested/component-identity.core.json"));
    // An empty component name is not a stamp: `<prefix><suffix>` names nothing.
    assert!(!is_stamp_file("component-identity..json"));
}

#[test]
fn an_absent_stamp_is_no_identity() {
    let dir = staged(&[("a.txt", b"a")]);
    assert_eq!(read_stamp(dir.path(), "core"), None);
    assert!(!has_stamp(dir.path()));
}

#[test]
fn a_stamp_from_another_schema_is_no_identity() {
    let dir = staged(&[("a.txt", b"a")]);
    let mut identity = ComponentIdentity::new("core", "3.3.11", "abc".to_string(), 0);
    identity.schema = SCHEMA + 1;
    std::fs::write(
        dir.path().join(stamp_file_name("core")),
        serde_json::to_vec(&identity).unwrap(),
    )
    .unwrap();
    assert_eq!(read_stamp(dir.path(), "core"), None);
    assert!(!has_stamp(dir.path()));
}

#[test]
fn an_unreadable_stamp_is_no_identity() {
    let dir = staged(&[("a.txt", b"a")]);
    std::fs::write(dir.path().join(stamp_file_name("core")), b"{not json").unwrap();
    assert_eq!(read_stamp(dir.path(), "core"), None);
}

#[test]
fn removing_a_stamp_leaves_no_identity() {
    let root = staged(&[("a.h", b"x")]);
    write_stamp(
        root.path(),
        &ComponentIdentity::new("core", "3.3.12", "id".to_string(), 0),
    )
    .unwrap();
    assert!(read_stamp(root.path(), "core").is_some());

    remove_stamp(root.path(), "core").unwrap();

    assert!(read_stamp(root.path(), "core").is_none());
    // Idempotent: withdrawing an identity twice is not an error.
    remove_stamp(root.path(), "core").unwrap();
}

// --- entries_survived: verifying a component merged into a shared directory ---

#[test]
fn entries_survived_accepts_a_superset() {
    let staged_tree = staged(&[
        ("flags/includes", b"-Iinclude"),
        ("include/hal.h", b"//hal"),
    ]);
    // The installed directory also holds the SDK variant's own files, which is
    // exactly why whole-tree equality cannot be the test here.
    let installed = staged(&[
        ("flags/includes", b"-Iinclude"),
        ("include/hal.h", b"//hal"),
        ("lib/libfreertos.a", b"freertos"),
    ]);
    assert!(entries_survived(staged_tree.path(), installed.path(), &[]).unwrap());
}

#[test]
fn entries_survived_ignores_the_staged_components_own_stamp() {
    let staged_tree = staged(&[("include/hal.h", b"//hal")]);
    write_stamp(
        staged_tree.path(),
        &ComponentIdentity::new("mcu-esp32c2", "c2_skeleton", "id".to_string(), 1),
    )
    .unwrap();
    let installed = staged(&[("include/hal.h", b"//hal")]);
    assert!(entries_survived(staged_tree.path(), installed.path(), &[]).unwrap());
}

#[test]
fn entries_survived_reports_a_changed_byte() {
    let staged_tree = staged(&[("include/hal.h", b"//hal")]);
    let installed = staged(&[("include/hal.h", b"//other")]);
    assert!(!entries_survived(staged_tree.path(), installed.path(), &[]).unwrap());
}

#[test]
fn entries_survived_reports_a_missing_entry() {
    let staged_tree = staged(&[("include/hal.h", b"//hal")]);
    let installed = staged(&[("include/other.h", b"//hal")]);
    assert!(!entries_survived(staged_tree.path(), installed.path(), &[]).unwrap());
}

#[test]
fn entries_survived_reports_a_missing_root() {
    let staged_tree = staged(&[("include/hal.h", b"//hal")]);
    let empty = tempfile::TempDir::new().unwrap();
    assert!(!entries_survived(staged_tree.path(), &empty.path().join("nope"), &[]).unwrap());
}

fn identity(component: &str, version: &str, content_id: &str, recipe: u32) -> ComponentIdentity {
    ComponentIdentity::new(component, version, content_id.to_string(), recipe)
}

#[test]
fn component_term_is_order_independent() {
    let core = identity("core", "3.3.12", "aaa", 0);
    let sdk = identity("sdk-esp32c3", "3.3.12", "bbb", 1);
    let skeleton = identity("mcu-esp32c3", "3.3.12", "ccc", 1);
    let forward = component_term(&[core.clone(), sdk.clone(), skeleton.clone()]);
    let backward = component_term(&[skeleton, sdk, core]);
    assert_eq!(forward, backward);
    assert!(forward.is_some());
}

#[test]
fn component_term_changes_with_a_content_id() {
    let before = component_term(&[identity("core", "3.3.12", "aaa", 0)]).unwrap();
    let after = component_term(&[identity("core", "3.3.12", "aab", 0)]).unwrap();
    assert_ne!(before, after);
}

#[test]
fn component_term_changes_with_a_recipe() {
    let before = component_term(&[identity("sdk-esp32c3", "3.3.12", "aaa", 1)]).unwrap();
    let after = component_term(&[identity("sdk-esp32c3", "3.3.12", "aaa", 2)]).unwrap();
    assert_ne!(before, after);
}

#[test]
fn component_term_changes_with_a_version() {
    let before = component_term(&[identity("core", "3.3.12", "aaa", 0)]).unwrap();
    let after = component_term(&[identity("core", "3.3.13", "aaa", 0)]).unwrap();
    assert_ne!(before, after);
}

#[test]
fn component_term_changes_with_a_component_name() {
    let before = component_term(&[identity("sdk-esp32c3", "3.3.12", "aaa", 1)]).unwrap();
    let after = component_term(&[identity("sdk-esp32c6", "3.3.12", "aaa", 1)]).unwrap();
    assert_ne!(before, after);
}

#[test]
fn component_term_of_no_identity_is_none() {
    assert_eq!(component_term(&[]), None);
}

#[test]
fn component_term_of_a_duplicated_component_is_none() {
    let first = identity("sdk-esp32c3", "3.3.12", "aaa", 1);
    let second = identity("sdk-esp32c3", "3.3.12", "bbb", 1);
    assert_eq!(component_term(&[first, second]), None);
}

#[test]
fn component_term_covers_every_component() {
    let one = component_term(&[identity("core", "3.3.12", "aaa", 0)]).unwrap();
    let two = component_term(&[
        identity("core", "3.3.12", "aaa", 0),
        identity("sdk-esp32c3", "3.3.12", "bbb", 1),
    ])
    .unwrap();
    assert_ne!(one, two);
}
