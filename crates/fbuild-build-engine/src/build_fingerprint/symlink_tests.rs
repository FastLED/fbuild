//! Watch-set coverage for symlinked local libraries (FastLED/fbuild#1753).
//!
//! A PlatformIO project reaches a working copy through `lib/<Lib> -> /elsewhere`.
//! The fast-path walk must see every file under the link target, or an edit
//! there never invalidates the warm path and the previous `firmware.bin` is
//! reused (and flashed) unchanged.

use std::fs;
use std::path::{Path, PathBuf};

use super::{FingerprintWatch, hash_watch_set, hash_watch_set_stamps};

fn make_watch(root: &Path, cache_file: &Path) -> FingerprintWatch {
    FingerprintWatch {
        cache_file: cache_file.to_path_buf(),
        root: root.to_path_buf(),
        extensions: vec!["cpp".to_string(), "h".to_string()],
        excludes: vec!["build".to_string()],
    }
}

fn make_symlinked_project(tmp: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let project = tmp.join("project");
    let lib = project.join("lib");
    fs::create_dir_all(&lib).unwrap();
    fs::write(project.join("main.cpp"), "int main() { return 0; }\n").unwrap();

    let target = tmp.join("fastled2");
    fs::create_dir_all(target.join("src/platforms/esp/32")).unwrap();
    fs::write(target.join("library.json"), "{}\n").unwrap();
    let header = target.join("src/platforms/esp/32/fastspi_esp32_idf.h");
    fs::write(&header, "#pragma once\n#define SPI_PATH 1\n").unwrap();

    fbuild_core::platform::fs::symlink_dir(&target, &lib.join("FastLED")).unwrap();
    (project, target, header)
}

#[test]
fn test_hash_watch_set_stamps_sees_edits_under_a_symlinked_library() {
    if fbuild_core::platform::host::is_windows() {
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let (project, _, header) = make_symlinked_project(tmp.path());
    let watch = make_watch(&project, &tmp.path().join("project.json"));

    let first = hash_watch_set_stamps(std::slice::from_ref(&watch)).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(&header, "#pragma once\n#define SPI_PATH 2\n").unwrap();
    let second = hash_watch_set_stamps(std::slice::from_ref(&watch)).unwrap();

    assert_ne!(
        first, second,
        "a header edited under a symlinked library must invalidate the watch set"
    );
}

#[test]
fn test_hash_watch_set_sees_edits_under_a_symlinked_library() {
    if fbuild_core::platform::host::is_windows() {
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let (project, _, header) = make_symlinked_project(tmp.path());
    let watch = make_watch(&project, &tmp.path().join("project.json"));

    let first = hash_watch_set(std::slice::from_ref(&watch)).unwrap();
    fs::write(&header, "#pragma once\n#define SPI_PATH 2\n").unwrap();
    let second = hash_watch_set(std::slice::from_ref(&watch)).unwrap();

    assert_ne!(first, second);
}

#[test]
fn test_hash_watch_set_stamps_hashes_a_symlinked_source_file() {
    if fbuild_core::platform::host::is_windows() {
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let src = tmp.path().join("src");
    fs::create_dir_all(&src).unwrap();
    let real = tmp.path().join("shared").join("main.cpp");
    fs::create_dir_all(real.parent().unwrap()).unwrap();
    fs::write(&real, "int main() { return 1; }\n").unwrap();
    fbuild_core::platform::fs::symlink_file(&real, &src.join("main.cpp")).unwrap();

    let watch = make_watch(&src, &tmp.path().join("project.json"));
    let first = hash_watch_set_stamps(std::slice::from_ref(&watch)).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(&real, "int main() { return 2; }\n").unwrap();
    let second = hash_watch_set_stamps(std::slice::from_ref(&watch)).unwrap();

    assert_ne!(first, second);
}

#[test]
fn test_hash_watch_set_stamps_tolerates_a_symlink_loop() {
    if fbuild_core::platform::host::is_windows() {
        return;
    }
    let tmp = tempfile::TempDir::new().unwrap();
    let src = tmp.path().join("src");
    let nested = src.join("nested");
    fs::create_dir_all(&nested).unwrap();
    fs::write(src.join("main.cpp"), "int main() { return 1; }\n").unwrap();
    // `src/nested/loop -> src` would recurse forever under a naive walk.
    fbuild_core::platform::fs::symlink_dir(&src, &nested.join("loop")).unwrap();

    let watch = make_watch(&src, &tmp.path().join("project.json"));
    let hash = hash_watch_set_stamps(std::slice::from_ref(&watch)).unwrap();

    assert!(!hash.is_empty());
}
