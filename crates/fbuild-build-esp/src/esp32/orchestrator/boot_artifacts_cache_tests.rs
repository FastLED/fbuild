//! Tests for the derived boot-artifact cache: key identity, sensitivity, and
//! the miss -> store -> hit round trip. No tool is ever spawned.

use super::{ArtifactKey, CACHE_DIR_NAME, content_hash};

fn write_tool(path: &std::path::Path, body: &[u8]) {
    std::fs::write(path, body).unwrap();
}

#[test]
fn key_is_stable_for_the_same_tool_and_parameters() {
    let tmp = tempfile::tempdir().unwrap();
    let tool = tmp.path().join("esptool");
    write_tool(&tool, b"tool-bytes");
    let left = ArtifactKey::new(&tool, &["esp32s3", "dio", "80m", "8MB"]).unwrap();
    let right = ArtifactKey::new(&tool, &["esp32s3", "dio", "80m", "8MB"]).unwrap();
    assert_eq!(left.path_in(tmp.path()), right.path_in(tmp.path()));
}

#[test]
fn key_changes_with_any_parameter_or_parameter_boundary() {
    let tmp = tempfile::tempdir().unwrap();
    let tool = tmp.path().join("esptool");
    write_tool(&tool, b"tool-bytes");
    let base = ArtifactKey::new(&tool, &["esp32s3", "dio", "80m"]).unwrap();
    let mode_changed = ArtifactKey::new(&tool, &["esp32s3", "qio", "80m"]).unwrap();
    let boundary_moved = ArtifactKey::new(&tool, &["esp32s3dio", "", "80m"]).unwrap();
    assert_ne!(base.path_in(tmp.path()), mode_changed.path_in(tmp.path()));
    assert_ne!(base.path_in(tmp.path()), boundary_moved.path_in(tmp.path()));
}

#[test]
fn key_follows_the_tool_stamp() {
    let tmp = tempfile::tempdir().unwrap();
    let tool = tmp.path().join("esptool");
    write_tool(&tool, b"tool-bytes");
    let before = ArtifactKey::new(&tool, &["esp32s3"]).unwrap();
    write_tool(&tool, b"tool-bytes-upgraded");
    let after = ArtifactKey::new(&tool, &["esp32s3"]).unwrap();
    assert_ne!(before.path_in(tmp.path()), after.path_in(tmp.path()));
}

#[test]
fn missing_tool_disables_the_cache() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(ArtifactKey::new(&tmp.path().join("absent"), &["esp32s3"]).is_none());
}

#[test]
fn miss_then_store_then_hit_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let tool = tmp.path().join("esptool");
    write_tool(&tool, b"tool-bytes");
    let produced = tmp.path().join("produced.bin");
    let destination = tmp.path().join("build").join("bootloader.bin");
    std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
    std::fs::write(&produced, b"image-bytes").unwrap();

    let key = ArtifactKey::new(&tool, &["esp32s3"]).unwrap();
    assert!(
        !key.materialize(tmp.path(), &destination),
        "cold cache must miss"
    );

    key.store(tmp.path(), &produced);
    assert!(
        tmp.path().join(CACHE_DIR_NAME).exists(),
        "store creates the cache dir"
    );

    std::fs::write(&destination, b"stale").unwrap();
    assert!(
        key.materialize(tmp.path(), &destination),
        "warm cache must hit"
    );
    assert_eq!(std::fs::read(&destination).unwrap(), b"image-bytes");
}

#[test]
fn store_is_best_effort_when_the_cache_root_is_unusable() {
    let tmp = tempfile::tempdir().unwrap();
    let tool = tmp.path().join("esptool");
    write_tool(&tool, b"tool-bytes");
    let produced = tmp.path().join("produced.bin");
    std::fs::write(&produced, b"image-bytes").unwrap();
    // A regular file where the cache root's directory must be: create_dir_all fails.
    let blocked = tmp.path().join("blocked");
    std::fs::write(&blocked, b"not a directory").unwrap();

    let key = ArtifactKey::new(&tool, &["esp32s3"]).unwrap();
    key.store(&blocked, &produced);
    assert!(!blocked.join(CACHE_DIR_NAME).exists());
}

#[test]
fn content_hash_ignores_location_and_tracks_bytes() {
    assert_eq!(
        content_hash(b"# Name, Type, SubType\n"),
        content_hash(b"# Name, Type, SubType\n")
    );
    assert_ne!(content_hash(b"a"), content_hash(b"b"));
    assert_eq!(content_hash(b"abc").len(), 32);
}
