//! Hermetic regression gate for #147 (cache must survive tar-extract / cross-runner restore).
//!
//! These tests pin the invariants whose union closes #147:
//!   * `hash_watch_set_stamps` is **content-derived**, not mtime-derived.
//!   * `relative_path_for_hash` keeps absolute workspace paths out of the hash.
//!   * `build_rebuild_signature` substitutes `compiler_identity` for the raw compiler path.
//!
//! If any of these regresses, this suite turns RED with a clear signal (instead of the
//! bench-fastled-examples warm-rebuild gate going slow on the next AC run).

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use filetime::{set_file_mtime, FileTime};
use tar::{Archive, Builder};
use tempfile::TempDir;

use fbuild_build::build_fingerprint::hash_watch_set_stamps;
use fbuild_build::compiler::build_rebuild_signature;
use fbuild_build::zccache::FingerprintWatch;

const SOURCES: &[(&str, &str)] = &[
    (
        "src/main.cpp",
        "#include <Arduino.h>\n#include \"foo.h\"\nvoid setup() { foo(); }\nvoid loop() {}\n",
    ),
    (
        "src/foo.cpp",
        "#include \"foo.h\"\nvoid foo() { /* deterministic */ }\n",
    ),
    ("src/foo.h", "#pragma once\nvoid foo();\n"),
];

fn populate_project(root: &Path) {
    for (rel, body) in SOURCES {
        let dest = root.join(rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(dest, body).unwrap();
    }
}

fn make_watch(cache_dir: &Path, root: &Path) -> FingerprintWatch {
    FingerprintWatch {
        cache_file: cache_dir.join("fingerprint"),
        root: root.to_path_buf(),
        extensions: vec!["cpp".to_string(), "h".to_string()],
        excludes: vec![],
    }
}

fn tar_directory(root: &Path) -> Vec<u8> {
    let mut builder = Builder::new(Vec::new());
    builder.append_dir_all("proj", root).unwrap();
    builder.into_inner().unwrap()
}

fn untar_into(bytes: &[u8], dest: &Path) {
    fs::create_dir_all(dest).unwrap();
    let mut archive = Archive::new(Cursor::new(bytes));
    archive.set_preserve_mtime(false);
    archive.unpack(dest).unwrap();
}

/// Force every regular file under `root` to a known mtime distinct from the source tree.
/// Models what a CI tar-extract / cross-runner restore looks like in practice: mtimes
/// either come back as "now" or are stamped with the cache restore time, never matching
/// the original build mtimes. If a future change re-introduces mtime into the watch hash,
/// this guarantees the two hashes diverge instead of accidentally matching.
fn stomp_mtimes(root: &Path, mtime: FileTime) {
    for entry in walkdir::WalkDir::new(root).into_iter().flatten() {
        if entry.file_type().is_file() {
            set_file_mtime(entry.path(), mtime).unwrap();
        }
    }
}

fn file_mtime(path: &Path) -> SystemTime {
    fs::metadata(path).unwrap().modified().unwrap()
}

#[test]
fn fingerprint_survives_tar_roundtrip() {
    let proj_a = TempDir::new().unwrap();
    let cache_a = TempDir::new().unwrap();
    populate_project(proj_a.path());
    let watch_a = make_watch(cache_a.path(), proj_a.path());
    let hash_a = hash_watch_set_stamps(std::slice::from_ref(&watch_a)).unwrap();

    let tarball = tar_directory(proj_a.path());
    let unpack_root = TempDir::new().unwrap();
    untar_into(&tarball, unpack_root.path());
    let proj_b = unpack_root.path().join("proj");

    let stomped = FileTime::from_unix_time(1_577_836_800, 0); // 2020-01-01 UTC
    stomp_mtimes(&proj_b, stomped);

    let mtime_a = file_mtime(&proj_a.path().join("src/main.cpp"));
    let mtime_b = file_mtime(&proj_b.join("src/main.cpp"));
    assert_ne!(
        mtime_a, mtime_b,
        "test setup invariant: extracted tree must have different mtimes than source \
         (otherwise we cannot prove mtime is excluded from the hash)"
    );

    let cache_b = TempDir::new().unwrap();
    let watch_b = make_watch(cache_b.path(), &proj_b);
    let hash_b = hash_watch_set_stamps(std::slice::from_ref(&watch_b)).unwrap();

    assert_eq!(
        hash_a, hash_b,
        "watch-set hash regressed: mtime or some other non-content input leaked into the hash. \
         See crates/fbuild-build/src/build_fingerprint/mod.rs::hash_watch_set_stamps_inner."
    );
}

#[test]
fn fingerprint_survives_workspace_relocation() {
    let proj_a = TempDir::new().unwrap();
    let cache_a = TempDir::new().unwrap();
    populate_project(proj_a.path());
    let watch_a = make_watch(cache_a.path(), proj_a.path());
    let hash_a = hash_watch_set_stamps(std::slice::from_ref(&watch_a)).unwrap();

    let tarball = tar_directory(proj_a.path());

    let different_parent = TempDir::new().unwrap();
    let unpack_root = different_parent
        .path()
        .join("nested")
        .join("run-c")
        .join("deeper");
    fs::create_dir_all(&unpack_root).unwrap();
    untar_into(&tarball, &unpack_root);
    let proj_c = unpack_root.join("proj");

    stomp_mtimes(&proj_c, FileTime::from_unix_time(1_609_459_200, 0)); // 2021-01-01 UTC

    assert_ne!(
        proj_a.path().parent(),
        proj_c.parent(),
        "test setup invariant: extracted project must live under a different parent path"
    );

    let cache_c = TempDir::new().unwrap();
    let watch_c = make_watch(cache_c.path(), &proj_c);
    let hash_c = hash_watch_set_stamps(std::slice::from_ref(&watch_c)).unwrap();

    assert_eq!(
        hash_a, hash_c,
        "watch-set hash regressed: absolute workspace path leaked into the hash. \
         See crates/fbuild-build/src/build_fingerprint/mod.rs::relative_path_for_hash."
    );
}

#[test]
fn compiler_signature_survives_toolchain_path_change() {
    let toolchain_a = TempDir::new().unwrap();
    let toolchain_b = TempDir::new().unwrap();

    let compiler_filename = if cfg!(windows) { "gcc.exe" } else { "gcc" };
    let path_a: PathBuf = toolchain_a.path().join(compiler_filename);
    let path_b: PathBuf = toolchain_b.path().join(compiler_filename);
    assert_ne!(
        path_a, path_b,
        "test setup invariant: the two compiler paths must differ as absolute path strings"
    );

    let flags = vec!["-O2".to_string(), "-DFOO=1".to_string()];
    let pre_flags = vec!["-Wall".to_string()];
    let extra_flags = vec!["-I/some/include".to_string()];
    let build_unflags: Vec<String> = Vec::new();

    let sig_a = build_rebuild_signature(&path_a, &flags, &pre_flags, &extra_flags, &build_unflags);
    let sig_b = build_rebuild_signature(&path_b, &flags, &pre_flags, &extra_flags, &build_unflags);

    assert_eq!(
        sig_a, sig_b,
        "build_rebuild_signature regressed: absolute compiler path leaked into the signature. \
         See crates/fbuild-build/src/compiler.rs::compiler_identity."
    );

    let alt_filename = if cfg!(windows) { "clang.exe" } else { "clang" };
    let path_c = toolchain_a.path().join(alt_filename);
    let sig_c = build_rebuild_signature(&path_c, &flags, &pre_flags, &extra_flags, &build_unflags);
    assert_ne!(
        sig_a, sig_c,
        "build_rebuild_signature collapsed two different compilers to the same signature; \
         compiler_identity must still distinguish on file stem."
    );
}
