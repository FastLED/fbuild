//! A chip slice must install, verify and stay chip-scoped (FastLED/fbuild#1944).

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};

use crate::disk_cache;
use crate::variants::SliceFetch;
use crate::{CacheSubdir, PackageBase};

const CHIP: &str = "esp32s3";
const OTHER_CHIP: &str = "esp32s2";

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// A `.tar.zst` whose `bin/` entry is what the validation closure checks.
fn tar_zst(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut tar = tar::Builder::new(Vec::new());
    for (name, content) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, name, *content)
            .expect("append tar entry");
    }
    zstd::encode_all(tar.into_inner().expect("finish tar").as_slice(), 3).expect("zstd")
}

/// Serves each path's body and records every path requested, so a test can
/// prove what fbuild did NOT download.
async fn serve(bodies: Vec<(&'static str, Vec<u8>)>) -> (String, Arc<Mutex<Vec<String>>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    let bodies: HashMap<&'static str, Vec<u8>> = bodies.into_iter().collect();
    let hits: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let seen = hits.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = request.split_whitespace().nth(1).unwrap_or("").to_string();
            seen.lock().expect("hits").push(path.clone());
            let body = bodies.get(path.as_str()).cloned().unwrap_or_default();
            let status = if bodies.contains_key(path.as_str()) {
                "200 OK"
            } else {
                "404 Not Found"
            };
            let header = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(header.as_bytes()).await;
            let _ = stream.write_all(&body).await;
            let _ = stream.shutdown().await;
        }
    });
    (base, hits)
}

fn base_with(tmp: &Path, cache_root: &Path, url: &str) -> PackageBase {
    PackageBase::with_cache_root(
        "esp32-xtensa-gcc",
        "14.2.0_20241119",
        url,
        "toolchain-xtensa-esp-elf",
        None,
        CacheSubdir::Toolchains,
        tmp,
        cache_root,
    )
}

fn validate_gcc(dir: &Path) -> fbuild_core::Result<()> {
    if dir.join("bin").join("xtensa-esp-elf-gcc").is_file() {
        return Ok(());
    }
    Err(fbuild_core::FbuildError::PackageError(
        "bin/xtensa-esp-elf-gcc missing".into(),
    ))
}

#[tokio::test]
async fn slice_installs_into_its_own_chip_path_and_never_fetches_the_whole_archive() {
    let payload = tar_zst(&[("bin/xtensa-esp-elf-gcc", b"#!/bin/sh\n")]);
    let (url, hits) = serve(vec![
        ("/slice.tar.zst", payload.clone()),
        ("/vendor.tar.zst", vec![b'X'; 4096]),
    ])
    .await;
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let cache_root = tmp.path().join("cache");
    let mut base = base_with(tmp.path(), &cache_root, &format!("{url}/vendor.tar.zst"));
    base.forced_slice = Some(SliceFetch {
        url: format!("{url}/slice.tar.zst"),
        sha256: sha256_hex(&payload),
        size: payload.len() as u64,
    });
    let base = base.with_slice_variant("ESP32-S3");

    let installed = base
        .staged_install(validate_gcc)
        .await
        .expect("slice install");

    let chip_path = base.chip_install_path().expect("chip path");
    assert_eq!(
        installed.as_path(),
        chip_path.as_path(),
        "a slice must not land in the shared dir"
    );
    assert!(
        installed
            .file_name()
            .expect("name")
            .to_string_lossy()
            .ends_with("+esp32s3"),
        "{}",
        installed.display()
    );
    assert!(disk_cache::paths::install_complete_sentinel(&installed).exists());
    assert!(base.is_cached());
    assert_eq!(base.installed_path().as_path(), installed);
    assert_eq!(
        hits.lock().expect("hits").clone(),
        vec!["/slice.tar.zst".to_string()],
        "the whole archive must never be requested once a slice is listed"
    );
}

#[tokio::test]
async fn corrupt_slice_fails_instead_of_downloading_the_whole_archive() {
    let payload = tar_zst(&[("bin/xtensa-esp-elf-gcc", b"#!/bin/sh\n")]);
    let (url, hits) = serve(vec![
        ("/slice.tar.zst", payload.clone()),
        ("/vendor.tar.zst", vec![b'X'; 4096]),
    ])
    .await;
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let cache_root = tmp.path().join("cache");
    let mut base = base_with(tmp.path(), &cache_root, &format!("{url}/vendor.tar.zst"));
    base.forced_slice = Some(SliceFetch {
        url: format!("{url}/slice.tar.zst"),
        sha256: sha256_hex(b"not this payload"),
        size: payload.len() as u64,
    });
    let base = base.with_slice_variant(CHIP);

    let error = base
        .staged_install(validate_gcc)
        .await
        .expect_err("a corrupt slice must fail the install");
    assert!(
        error.to_string().contains("checksum mismatch"),
        "unexpected error: {error}"
    );
    assert_eq!(
        hits.lock().expect("hits").clone(),
        vec!["/slice.tar.zst".to_string()],
        "a listed-but-broken slice must not silently become a full download"
    );
    assert!(!base.chip_install_path().expect("chip path").exists());
}

#[tokio::test]
async fn a_shared_whole_install_still_serves_every_chip() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let cache_root = tmp.path().join("cache");
    let base = base_with(tmp.path(), &cache_root, "http://127.0.0.1:9/unused.tar.zst");
    // A host that installed the whole toolchain before slicing existed.
    let shared = base.install_path();
    std::fs::create_dir_all(shared.join("bin")).expect("create shared");
    std::fs::write(shared.join("bin").join("xtensa-esp-elf-gcc"), b"").expect("write gcc");
    std::fs::write(disk_cache::paths::install_complete_sentinel(&shared), b"").expect("sentinel");

    for chip in [CHIP, OTHER_CHIP] {
        let mut scoped = base_with(tmp.path(), &cache_root, "http://127.0.0.1:9/unused.tar.zst");
        scoped.set_slice_variant(chip);
        assert!(
            scoped.is_cached(),
            "{chip} must see the shared whole install"
        );
        assert_eq!(scoped.installed_path().as_path(), shared, "{chip}");
        // No download: the shared tree is returned as-is.
        assert_eq!(
            scoped.staged_install(validate_gcc).await.expect("hit"),
            shared
        );
    }
}

#[tokio::test]
async fn no_published_slice_falls_back_to_the_whole_archive() {
    let payload = tar_zst(&[("bin/xtensa-esp-elf-gcc", b"#!/bin/sh\n")]);
    let (url, hits) = serve(vec![("/vendor.tar.zst", payload)]).await;
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let cache_root = tmp.path().join("cache");
    let base = base_with(tmp.path(), &cache_root, &format!("{url}/vendor.tar.zst"))
        .with_slice_variant(CHIP);

    let installed = base
        .staged_install(validate_gcc)
        .await
        .expect("whole install");
    assert_eq!(
        installed.as_path(),
        base.chip_install_path().expect("chip path").as_path()
    );
    assert_eq!(
        hits.lock().expect("hits").clone(),
        vec!["/vendor.tar.zst".to_string()]
    );
}

#[test]
fn chip_paths_are_per_chip_and_never_the_shared_path() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let cache_root = tmp.path().join("cache");
    let base = base_with(tmp.path(), &cache_root, "http://127.0.0.1:9/x.tar.zst");
    let shared = base.install_path();
    let mut s3 = base_with(tmp.path(), &cache_root, "http://127.0.0.1:9/x.tar.zst");
    s3.set_slice_variant("ESP32-S3");
    let mut s2 = base_with(tmp.path(), &cache_root, "http://127.0.0.1:9/x.tar.zst");
    s2.set_slice_variant("esp32_s2");
    let s3_path = s3.chip_install_path().expect("s3 path");
    let s2_path = s2.chip_install_path().expect("s2 path");

    assert_ne!(s3_path.as_path(), shared);
    assert_ne!(s3_path, s2_path);
    assert_eq!(
        s3_path.file_name().unwrap().to_string_lossy(),
        format!("{}+esp32s3", shared.file_name().unwrap().to_string_lossy()),
        "the chip path must be a sibling of the shared path, so sentinel and LRU keying keep working"
    );
    // A chip install is invisible to the other chip.
    std::fs::create_dir_all(s3_path.join("bin")).expect("create");
    std::fs::write(s3_path.join("bin").join("xtensa-esp-elf-gcc"), b"").expect("gcc");
    std::fs::write(disk_cache::paths::install_complete_sentinel(&s3_path), b"").expect("sentinel");
    assert!(s3.is_cached());
    assert!(s3.installed_path().to_string_lossy().ends_with("+esp32s3"));
    assert!(
        !s2.is_cached(),
        "one chip's slice must not satisfy another chip"
    );
    assert_eq!(s2.installed_path().as_path(), shared);
}
