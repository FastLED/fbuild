use super::toolchain_gcc_ar_tests::{serve_once, zip_bytes};
use super::*;

#[tokio::test]
async fn failed_validation_never_commits_a_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let url = serve_once(zip_bytes("file", b"payload")).await;
    let base = PackageBase::with_cache_root(
        "tool",
        "1",
        &url,
        "failure-test",
        None,
        CacheSubdir::Toolchains,
        tmp.path(),
        &tmp.path().join("cache"),
    );
    let installed = base.install_path();
    let result = base
        .staged_install(|_| {
            Err(fbuild_core::FbuildError::PackageError(
                "rejected fixture".into(),
            ))
        })
        .await;
    assert!(result.is_err());
    assert!(!installed.exists());
    let staging = installed.with_file_name(format!(
        "{}_staging",
        installed.file_name().unwrap().to_string_lossy()
    ));
    assert!(staging.join("file").exists());
    assert!(
        !staging
            .join(fbuild_core::content_manifest::SIDECAR)
            .exists()
    );
}

#[tokio::test]
async fn failed_atomic_rename_removes_staged_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let url = serve_once(zip_bytes("file", b"payload")).await;
    let base = PackageBase::with_cache_root(
        "tool",
        "1",
        &url,
        "rename-failure",
        None,
        CacheSubdir::Toolchains,
        tmp.path(),
        &tmp.path().join("cache"),
    );
    let installed = base.install_path();
    let result = base
        .staged_install(|staging| {
            assert!(
                !staging
                    .join(fbuild_core::content_manifest::SIDECAR)
                    .exists()
            );
            std::fs::create_dir_all(&installed)?;
            std::fs::write(installed.join("occupied"), b"prevent replacement")?;
            Ok(())
        })
        .await;
    assert!(result.is_err());
    let staging = installed.with_file_name(format!(
        "{}_staging",
        installed.file_name().unwrap().to_string_lossy()
    ));
    assert!(staging.join("file").exists());
    assert!(
        !staging
            .join(fbuild_core::content_manifest::SIDECAR)
            .exists()
    );
    assert!(
        !installed
            .join(fbuild_core::content_manifest::SIDECAR)
            .exists()
    );
}
