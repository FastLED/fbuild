//! Call-site fixtures for the prebuilt tree policy (#1882): real tar.zst bytes,
//! a real HTTP server, and the cache layout the sweep must cover.
use crate::disk_cache::paths::prebuilt_root;
use crate::prebuilt::*;
use fbuild_core::path::NormalizedPath;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, SystemTime};
use tar::{Builder, Header};

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn tar_zst(name: &str, body: &[u8]) -> Vec<u8> {
    let encoder = zstd::Encoder::new(Vec::new(), 3).unwrap();
    let mut builder = Builder::new(encoder);
    let mut header = Header::new_gnu();
    header.set_size(body.len() as u64);
    header.set_mode(0o644);
    header.set_entry_type(tar::EntryType::Regular);
    header.set_cksum();
    builder.append_data(&mut header, name, body).unwrap();
    let mut encoder = builder.into_inner().unwrap();
    encoder.flush().unwrap();
    encoder.finish().unwrap()
}

/// A loopback server that answers every request with `body` and counts calls.
async fn server(body: Vec<u8>) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let count = Arc::new(AtomicUsize::new(0));
    let calls = count.clone();
    let app = axum::Router::new().fallback(move || {
        let body = body.clone();
        let calls = calls.clone();
        async move {
            calls.fetch_add(1, Ordering::SeqCst);
            body
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (address, count, handle)
}

fn query(fp: &str) -> Query {
    Query {
        framework: "arduino-esp32".into(),
        arch: "esp32".into(),
        abi: "gcc@1".into(),
        version: "3.1.1".into(),
        fp: fp.into(),
    }
}

fn asset(bytes: &[u8], urls: Vec<String>) -> Asset {
    Asset {
        filename: "prebuilt-framework.tar.zst".into(),
        size_bytes: bytes.len() as u64,
        sha256: digest(bytes),
        urls,
        provides: vec![],
        parts: vec![],
    }
}

/// The relative path of the tree below `prebuilt/`, as a slash string.
fn relative(cache_root: &std::path::Path, query: &Query) -> String {
    let root = tree_root(cache_root, query).unwrap();
    let base = NormalizedPath::new(prebuilt_root(cache_root));
    root.relative_to(&base).unwrap().display_slash()
}

#[test]
fn the_tree_path_is_the_same_key_the_bucket_uses() {
    let temp = tempfile::tempdir().unwrap();
    let cache = temp.path().join("cache");
    let query = query(&"b".repeat(64));
    let mut asset = asset(b"x", vec![]);
    let urls = operator_urls(&asset, &query, Some("https://mirror.example/{path}"), true);
    assert_eq!(urls.len(), 1);
    // `operator_urls` builds `prebuilt/<fw>/<version>/<arch>/fp-<sha16>/<file>`;
    // the tree lives at exactly that directory, so one identity drives both.
    let expected = format!(
        "prebuilt/{}/prebuilt-framework.tar.zst",
        relative(cache.as_path(), &query)
    );
    assert_eq!(
        urls[0],
        format!("https://mirror.example/{expected}"),
        "bucket key and tree key must agree"
    );
    asset.urls.clear();
    assert_eq!(
        relative(cache.as_path(), &query),
        "arduino-esp32/3.1.1/esp32/fp-bbbbbbbbbbbbbbbb"
    );
}

#[test]
fn a_fingerprint_that_is_not_64_hex_gets_no_path() {
    let temp = tempfile::tempdir().unwrap();
    for fp in ["b".repeat(63), "B".repeat(64), String::new()] {
        assert!(matches!(
            tree_root(temp.path(), &query(&fp)),
            Err(ObtainError::InvalidFingerprint(_))
        ));
    }
}

#[test]
fn a_key_component_can_never_steer_the_write_elsewhere() {
    let temp = tempfile::tempdir().unwrap();
    let fp = "b".repeat(64);
    for (field, value) in [
        ("framework", "../escape"),
        ("framework", ""),
        ("version", "a/b"),
        ("arch", "a\\b"),
        ("version", ".."),
        ("arch", "C:\\win"),
    ] {
        let mut query = query(&fp);
        match field {
            "framework" => query.framework = value.into(),
            "version" => query.version = value.into(),
            _ => query.arch = value.into(),
        }
        match tree_root(temp.path(), &query) {
            Err(ObtainError::InvalidComponent { field: named, .. }) => assert_eq!(named, field),
            other => panic!("{field}={value:?} was accepted: {other:?}"),
        }
    }
}

#[tokio::test]
async fn a_complete_tree_is_reused_without_a_single_request() {
    let archive = tar_zst("lib/libesp32.a", b"example");
    let (url, calls, task) = server(archive.clone()).await;
    let temp = tempfile::tempdir().unwrap();
    let cache = temp.path().join("cache");
    let query = query(&"b".repeat(64));
    let asset = asset(&archive, vec![url]);
    let fetcher = Fetcher::new(&temp.path().join("downloads"), reqwest::Client::new());

    let first = obtain(cache.as_path(), &query, &asset, &fetcher)
        .await
        .unwrap();
    assert!(first.downloaded);
    assert_eq!(first.tree.root, tree_root(cache.as_path(), &query).unwrap());
    assert_eq!(
        std::fs::read_to_string(first.tree.root.join("lib/libesp32.a").as_path()).unwrap(),
        "example"
    );

    let second = obtain(cache.as_path(), &query, &asset, &fetcher)
        .await
        .unwrap();
    assert!(!second.downloaded, "a marked tree must not be re-fetched");
    assert_eq!(second.tree, first.tree);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    task.abort();
}

/// Stamp the tree's marker as last used `age` ago, the way an idle tree reads.
fn backdate_marker(tree_root: &NormalizedPath, age: Duration) {
    let marker = tree_root.join(MARKER);
    std::fs::File::options()
        .write(true)
        .open(marker.as_path())
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(SystemTime::now() - age))
        .unwrap();
}

#[tokio::test]
async fn a_reuse_records_itself_as_the_last_use() {
    let archive = tar_zst("lib/libesp32.a", b"example");
    let (url, calls, task) = server(archive.clone()).await;
    let temp = tempfile::tempdir().unwrap();
    let cache = temp.path().join("cache");
    let query = query(&"b".repeat(64));
    let asset = asset(&archive, vec![url]);
    let fetcher = Fetcher::new(&temp.path().join("downloads"), reqwest::Client::new());

    let first = obtain(cache.as_path(), &query, &asset, &fetcher)
        .await
        .unwrap();
    // An idle tree: far past the LRU's grace period, so eviction would take it.
    backdate_marker(&first.tree.root, Duration::from_secs(24 * 60 * 60));
    let stale = std::fs::symlink_metadata(first.tree.root.join(MARKER).as_path())
        .unwrap()
        .modified()
        .unwrap();

    let second = obtain(cache.as_path(), &query, &asset, &fetcher)
        .await
        .unwrap();
    assert!(!second.downloaded);
    let used = std::fs::symlink_metadata(first.tree.root.join(MARKER).as_path())
        .unwrap()
        .modified()
        .unwrap();
    assert!(
        used.duration_since(stale).unwrap() > Duration::from_secs(60 * 60),
        "a reuse must bump the marker's last-use time, or the LRU evicts a tree that is in use"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1, "the reuse stays offline");
    task.abort();
}

#[tokio::test]
async fn a_different_archive_under_the_same_identity_is_refused_untouched() {
    let archive = tar_zst("lib/libesp32.a", b"example");
    let (url, calls, task) = server(archive.clone()).await;
    let temp = tempfile::tempdir().unwrap();
    let cache = temp.path().join("cache");
    let query = query(&"b".repeat(64));
    let asset = asset(&archive, vec![url]);
    let fetcher = Fetcher::new(&temp.path().join("downloads"), reqwest::Client::new());

    // A tree this archive did not produce already sits at the identity's path.
    let dest = tree_root(cache.as_path(), &query).unwrap();
    std::fs::create_dir_all(dest.as_path()).unwrap();
    let existing = "c".repeat(64);
    std::fs::write(
        dest.join(MARKER).as_path(),
        json!({"sha256": existing, "size_bytes": 7, "entries": 1}).to_string(),
    )
    .unwrap();

    match obtain(cache.as_path(), &query, &asset, &fetcher).await {
        Err(ObtainError::IdentityConflict {
            path,
            existing: named,
            expected,
        }) => {
            assert_eq!(path, dest.display_slash());
            assert_eq!(named.as_deref(), Some(existing.as_str()));
            assert_eq!(expected, asset.sha256);
        }
        other => panic!("expected an identity conflict, got {other:?}"),
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "a refusal must cost no download"
    );
    assert!(dest.join(MARKER).as_path().is_file(), "tree left untouched");
    task.abort();
}

#[tokio::test]
async fn a_marker_that_does_not_parse_is_a_conflict_not_a_reuse() {
    let archive = tar_zst("lib/libesp32.a", b"example");
    let (url, calls, task) = server(archive.clone()).await;
    let temp = tempfile::tempdir().unwrap();
    let cache = temp.path().join("cache");
    let query = query(&"b".repeat(64));
    let asset = asset(&archive, vec![url]);
    let fetcher = Fetcher::new(&temp.path().join("downloads"), reqwest::Client::new());

    let dest = tree_root(cache.as_path(), &query).unwrap();
    std::fs::create_dir_all(dest.as_path()).unwrap();
    std::fs::write(dest.join(MARKER).as_path(), b"not json").unwrap();

    match obtain(cache.as_path(), &query, &asset, &fetcher).await {
        Err(ObtainError::IdentityConflict { existing, .. }) => assert_eq!(existing, None),
        other => panic!("expected an identity conflict, got {other:?}"),
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    task.abort();
}

#[tokio::test]
async fn the_tree_sweep_reaps_an_abandoned_staging_sibling_of_a_tree_path() {
    let temp = tempfile::tempdir().unwrap();
    let cache = NormalizedPath::new(temp.path().join("cache"));
    let query = query(&"b".repeat(64));
    let dest = tree_root(cache.as_path(), &query).unwrap();
    // Extraction stages beside the destination; the sweep must visit that depth.
    let staging =
        NormalizedPath::new(dest.as_path().parent().unwrap()).join(".0123456789abcdef.0.staging");
    std::fs::create_dir_all(staging.as_path()).unwrap();
    std::fs::write(staging.join("partial.a").as_path(), b"debris").unwrap();

    let report = sweep_staging_tree_async(cache.clone(), Duration::ZERO)
        .await
        .unwrap();
    assert_eq!(report.removed, 1, "the staging sibling must be reaped");
    assert_eq!(report.kept_live + report.kept_marked, 0);
    assert!(!staging.as_path().exists());
}
