//! `staged_install` must never hand back a directory that lacks the
//! `.install_complete` sentinel (FastLED/fbuild#2043).
//!
//! A stray write, a CI cache restored without the sentinel, or a legacy
//! partial tree can leave a foreign directory at the final install path.
//! `is_cached()` already reports such a directory as "not installed"; the
//! install path must agree and repair it instead of trusting it forever.

use std::path::Path;

use crate::chip_slice_tests::{serve, tar_zst};
use crate::{CacheSubdir, PackageBase, disk_cache};

fn base_for(tmp: &Path, cache_root: &Path, url: &str) -> PackageBase {
    PackageBase::with_cache_root(
        "framework-test",
        "4.5.3",
        url,
        "framework-test-key",
        None,
        CacheSubdir::Platforms,
        tmp,
        cache_root,
    )
}

fn validate_core(dir: &Path) -> fbuild_core::Result<()> {
    if dir.join("cores").join("Arduino.h").is_file() {
        return Ok(());
    }
    Err(fbuild_core::FbuildError::PackageError(
        "cores/Arduino.h missing".into(),
    ))
}

/// Every entry beside `install_path` other than `install_path` itself.
fn siblings_of(install_path: &Path) -> Vec<std::ffi::OsString> {
    let name = install_path.file_name().expect("name");
    std::fs::read_dir(install_path.parent().expect("parent"))
        .expect("read parent")
        .map(|e| e.expect("entry").file_name())
        .filter(|entry| entry != name)
        .collect()
}

#[tokio::test]
async fn staged_install_reinstalls_dir_without_sentinel() {
    let payload = tar_zst(&[("cores/Arduino.h", b"// core\n")]);
    let (url, hits) = serve(vec![("/core.tar.zst", payload)]).await;
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let cache_root = tmp.path().join("cache");
    let base = base_for(tmp.path(), &cache_root, &format!("{url}/core.tar.zst"));

    // The exact shape seen in the field: a stray variant file and nothing else.
    let install_path = base.install_path();
    let stray = install_path.join("variants/x/memmap_default.ld");
    std::fs::create_dir_all(stray.parent().expect("parent")).expect("mkdir stray");
    std::fs::write(&stray, b"stray").expect("write stray");
    assert!(!base.is_cached(), "precondition: stray dir is not cached");

    let installed = base
        .staged_install(validate_core)
        .await
        .expect("an incomplete dir must be repaired, not returned");

    assert_eq!(installed, install_path);
    assert!(
        installed.join("cores/Arduino.h").is_file(),
        "the package must actually be extracted"
    );
    assert!(
        disk_cache::paths::install_complete_sentinel(&installed).exists(),
        "a repaired install must carry the sentinel"
    );
    assert!(
        !stray.exists(),
        "the foreign tree must not survive the repair"
    );
    assert!(base.is_cached());
    assert_eq!(
        hits.lock().expect("hits").as_slice(),
        ["/core.tar.zst"],
        "the archive must be downloaded exactly once"
    );
    let leftovers = siblings_of(&install_path);
    assert!(
        leftovers.is_empty(),
        "no quarantine left behind: {leftovers:?}"
    );
}

#[tokio::test]
async fn staged_install_replaces_a_file_squatting_on_the_install_path() {
    let payload = tar_zst(&[("cores/Arduino.h", b"// core\n")]);
    let (url, _hits) = serve(vec![("/core.tar.zst", payload)]).await;
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let cache_root = tmp.path().join("cache");
    let base = base_for(tmp.path(), &cache_root, &format!("{url}/core.tar.zst"));

    let install_path = base.install_path();
    std::fs::create_dir_all(install_path.parent().expect("parent")).expect("mkdir");
    std::fs::write(&install_path, b"not a directory").expect("write squatter");

    let installed = base
        .staged_install(validate_core)
        .await
        .expect("a file at the install path must be replaced");

    assert!(installed.join("cores/Arduino.h").is_file());
    assert!(base.is_cached());
}
