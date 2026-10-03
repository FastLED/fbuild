//! Staged installs restore the exec bit on APE (cosmocc) host tools.

use super::toolchain_gcc_ar_tests::serve_once;
use super::*;

/// A zip whose entries carry no exec bits (the `zip` writer default is
/// 0644, like DOS-attribute archives) must still install an APE host tool
/// as executable, while leaving non-APE files exactly as extracted.
#[tokio::test]
async fn staged_install_marks_ape_tools_executable() {
    use std::io::{Cursor, Write};
    use zip::write::SimpleFileOptions;

    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file("pkg/bin/tool.com", SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"MZqFpD='\n#!/bin/sh\n").unwrap();
    zip.start_file("pkg/bin/readme.txt", SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"not a tool").unwrap();
    let bytes = zip.finish().unwrap().into_inner();

    let tmp = tempfile::TempDir::new().unwrap();
    // Plain extraction (not only staged installs) restores the APE exec
    // bit too, and leaves other 0644 entries alone.
    let archive = tmp.path().join("ref.zip");
    std::fs::write(&archive, &bytes).unwrap();
    let reference = tmp.path().join("ref");
    std::fs::create_dir_all(&reference).unwrap();
    extractor::extract(&archive, &reference).unwrap();
    if !fbuild_core::platform::host::current().is_windows() {
        let ref_ape = std::fs::metadata(reference.join("pkg/bin/tool.com")).unwrap();
        assert!(fbuild_core::platform::fs::is_executable(&ref_ape));
        let ref_txt = std::fs::metadata(reference.join("pkg/bin/readme.txt")).unwrap();
        assert!(!fbuild_core::platform::fs::is_executable(&ref_txt));
    }

    let url = serve_once(bytes).await;
    let base = PackageBase::with_cache_root(
        "ape-tool",
        "1.0",
        &url,
        "ape-tool",
        None,
        CacheSubdir::Toolchains,
        tmp.path(),
        &tmp.path().join("cache"),
    );
    let installed = base
        .staged_install(|staging| {
            let meta = std::fs::metadata(staging.join("pkg/bin/tool.com")).unwrap();
            assert!(
                fbuild_core::platform::fs::is_executable(&meta),
                "APE must be executable before validation runs"
            );
            Ok(())
        })
        .await
        .expect("staged install should succeed");

    assert!(
        ape_perms::is_repaired(&installed),
        "a fresh install records its APE scan so it is never repaired again"
    );
    let ape = std::fs::metadata(installed.join("pkg/bin/tool.com")).unwrap();
    assert!(fbuild_core::platform::fs::is_executable(&ape));
    let txt = std::fs::metadata(installed.join("pkg/bin/readme.txt")).unwrap();
    let ref_txt = std::fs::metadata(reference.join("pkg/bin/readme.txt")).unwrap();
    assert_eq!(
        txt.permissions(),
        ref_txt.permissions(),
        "non-APE files keep their extracted permissions"
    );
}
