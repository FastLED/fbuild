//! End-to-end integration test for the `.lnk` resource pipeline.
//!
//! Spins up an in-process axum HTTP server serving canned bytes, writes a
//! `.lnk` pointing at it, runs the full scan → resolve → materialize flow
//! against a fresh disk cache, and asserts the materialized file has the
//! expected content.
//!
//! Exercises the parts that the unit tests can't reach without network:
//! the actual `download_file` call inside the resolver, sha256 verify on
//! a fetched blob, and end-to-end materialization including hardlink/copy
//! into the build tree.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Bytes;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use sha2::{Digest, Sha256};

use fbuild_packages::lnk::{materialize_all, scan_for_lnk};
use fbuild_packages::DiskCache;

/// Hard wall-clock budget for each lnk e2e tokio test (FastLED/fbuild#806).
///
/// The audit flagged these because `materialize_all` (sync, internally calls
/// `download_file`) and `resolve` are invoked from inside `#[tokio::test]`
/// with no enclosing `tokio::time::timeout`. Server lives in-process on
/// loopback, so 15 s is generous — a wedge in the resolver, a missing
/// `server_handle.abort()`, or a deadlocked sha-verify path all surface as
/// a hard test failure within 15 s instead of letting CI sit on its 6 h job
/// budget.
///
/// Caveat: the materialize/resolve calls are sync, so a true deadlock inside
/// them can't be cancelled by `tokio::time::timeout` — the test task survives
/// the timeout panic on its blocking thread. But the wrap still serves the
/// audit's goal: it bounds the *test's* wall-clock so CI fails fast rather
/// than hanging the whole job. Promoting these to true cancellation requires
/// `spawn_blocking`, which is overkill for loopback-only fixtures.
const LNK_TEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

fn tempdir() -> tempfile::TempDir {
    tempfile::TempDir::new_in(fbuild_paths::temp_subdir("fbuild-packages-lnk-e2e-tests")).unwrap()
}

/// Run `body` under `LNK_TEST_TIMEOUT`; panic with a clear message if it
/// trips. Keeps each `#[tokio::test]` body free of right-shift indentation.
async fn with_timeout<F, Fut>(name: &str, body: F)
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    match tokio::time::timeout(LNK_TEST_TIMEOUT, body()).await {
        Ok(()) => {}
        Err(_) => panic!(
            "lnk e2e test `{name}` exceeded {:.0}s budget — see #806 audit",
            LNK_TEST_TIMEOUT.as_secs_f64()
        ),
    }
}

fn sha256_of(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

/// Spawn a tiny axum server on a free localhost port. Returns the bound
/// port and a future that drives the server. The server has one route:
/// `GET /<name>` returns the bytes registered under `name`.
async fn spawn_test_server(blobs: Vec<(String, Vec<u8>)>) -> (u16, tokio::task::JoinHandle<()>) {
    let blobs: Arc<Vec<(String, Vec<u8>)>> = Arc::new(blobs);
    let blobs_for_handler = Arc::clone(&blobs);

    let app = Router::new().route(
        "/:name",
        get(
            move |axum::extract::Path(name): axum::extract::Path<String>| {
                let blobs = Arc::clone(&blobs_for_handler);
                async move {
                    for (n, bytes) in blobs.iter() {
                        if n == &name {
                            return (StatusCode::OK, Bytes::from(bytes.clone())).into_response();
                        }
                    }
                    (StatusCode::NOT_FOUND, "not found").into_response()
                }
            },
        ),
    );

    // Bind to port 0 to get a free port from the OS.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    // Tiny delay to ensure server is accepting before tests fire.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    (port, handle)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lnk_pipeline_e2e_fetches_verifies_and_materializes() {
    with_timeout(
        "lnk_pipeline_e2e_fetches_verifies_and_materializes",
        || async {
            let blob_bytes = b"hello from the lnk e2e test".to_vec();
            let blob_sha = sha256_of(&blob_bytes);

            let (port, server_handle) =
                spawn_test_server(vec![("asset.bin".to_string(), blob_bytes.clone())]).await;
            let url = format!("http://127.0.0.1:{port}/asset.bin");

            // Set up a project tree with one .lnk pointing at our test server.
            let work = tempdir();
            let src_root = work.path().join("src");
            let build_dir = work.path().join("build/resources");
            let cache_dir = work.path().join("cache");

            let lnk_path = src_root.join("data/asset.bin.lnk");
            std::fs::create_dir_all(lnk_path.parent().unwrap()).unwrap();
            let lnk_json = format!(
                r#"{{"v":1,"url":"{url}","sha256":"{blob_sha}","size":{}}}"#,
                blob_bytes.len()
            );
            std::fs::write(&lnk_path, &lnk_json).unwrap();

            let cache = DiskCache::open_at(&cache_dir).unwrap();

            // Scan finds the lnk.
            let discovered = scan_for_lnk(&src_root).unwrap();
            assert_eq!(discovered.len(), 1, "scanner should find the one .lnk");
            assert_eq!(discovered[0].lnk.sha256, blob_sha);

            // Materialize fetches + verifies + writes into the build tree.
            let materialized = materialize_all(&discovered, &src_root, &build_dir, &cache).unwrap();
            assert_eq!(materialized.len(), 1);

            let target = build_dir.join("data/asset.bin");
            assert!(
                target.exists(),
                "materialized file should exist at {}",
                target.display()
            );
            let got = std::fs::read(&target).unwrap();
            assert_eq!(got, blob_bytes, "materialized bytes should match source");

            // Second materialization is a cache hit — no network would be required.
            // (We could shut down the server here to *prove* it, but the cleanest
            // assertion is just that it succeeds and the bytes are still right.)
            let materialized_again =
                materialize_all(&discovered, &src_root, &build_dir, &cache).unwrap();
            assert_eq!(materialized_again.len(), 1);
            assert_eq!(std::fs::read(&target).unwrap(), blob_bytes);

            server_handle.abort();
        },
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lnk_pipeline_rejects_sha_mismatch() {
    with_timeout("lnk_pipeline_rejects_sha_mismatch", || async {
        let blob_bytes = b"actual bytes from server".to_vec();
        let wrong_sha = sha256_of(b"different content"); // claims something else

        let (port, server_handle) =
            spawn_test_server(vec![("x.bin".to_string(), blob_bytes.clone())]).await;
        let url = format!("http://127.0.0.1:{port}/x.bin");

        let work = tempdir();
        let src_root = work.path().join("src");
        let build_dir = work.path().join("build");
        let cache_dir = work.path().join("cache");

        let lnk_path = src_root.join("x.bin.lnk");
        std::fs::create_dir_all(&src_root).unwrap();
        std::fs::write(
            &lnk_path,
            format!(r#"{{"v":1,"url":"{url}","sha256":"{wrong_sha}"}}"#),
        )
        .unwrap();

        let cache = DiskCache::open_at(&cache_dir).unwrap();
        let discovered = scan_for_lnk(&src_root).unwrap();
        assert_eq!(discovered.len(), 1);

        let result = materialize_all(&discovered, &src_root, &build_dir, &cache);
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("sha256 mismatch"),
            "expected sha mismatch error, got: {err}"
        );

        // Build target should NOT exist after a failed verify.
        let target = build_dir.join("x.bin");
        assert!(
            !target.exists(),
            "target should not be materialized on failed verify"
        );

        server_handle.abort();
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lnk_pipeline_handles_404() {
    with_timeout("lnk_pipeline_handles_404", || async {
        let (port, server_handle) = spawn_test_server(vec![]).await;
        let url = format!("http://127.0.0.1:{port}/nope.bin");
        // 404 still produces *some* response body; sha matches that won't be
        // ours. Easier: just refer to a non-existent route and let the
        // download succeed (returning the 404 page) but verify will fail.

        let work = tempdir();
        let src_root = work.path().join("src");
        let build_dir = work.path().join("build");
        let cache_dir = work.path().join("cache");
        std::fs::create_dir_all(&src_root).unwrap();

        // Sha that won't match the 404 page.
        let bogus_sha = "0".repeat(64);
        std::fs::write(
            src_root.join("nope.bin.lnk"),
            format!(r#"{{"v":1,"url":"{url}","sha256":"{bogus_sha}"}}"#),
        )
        .unwrap();

        let cache = DiskCache::open_at(&cache_dir).unwrap();
        let discovered = scan_for_lnk(&src_root).unwrap();
        assert_eq!(discovered.len(), 1);

        // Either the downloader bails on the non-2xx, or we bail on sha verify.
        // Both are acceptable failure modes — the assertion is just "errors out".
        let result = materialize_all(&discovered, &src_root, &build_dir, &cache);
        assert!(
            result.is_err(),
            "expected error for unreachable/missing blob"
        );

        server_handle.abort();
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lnk_resolver_cache_hit_skips_network_on_second_call() {
    with_timeout(
        "lnk_resolver_cache_hit_skips_network_on_second_call",
        || async {
            let blob_bytes = b"cache me".to_vec();
            let sha = sha256_of(&blob_bytes);

            let (port, server_handle) =
                spawn_test_server(vec![("y.bin".to_string(), blob_bytes.clone())]).await;
            let url = format!("http://127.0.0.1:{port}/y.bin");

            let work = tempdir();
            let cache_dir = work.path().join("cache");
            let cache = DiskCache::open_at(&cache_dir).unwrap();

            // First call: cache miss → download.
            let lnk = fbuild_packages::LnkFile {
                version: 1,
                url: url.clone(),
                sha256: sha.clone(),
                size: None,
                extract: fbuild_packages::ExtractMode::File,
            };
            let r1 = fbuild_packages::lnk::resolve(&lnk, &cache).unwrap();
            assert_eq!(r1.sha256, sha);
            let blob_path: PathBuf = r1.path.clone();
            assert!(blob_path.exists());

            // Now shut down the server so we *prove* the second call is offline.
            server_handle.abort();
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;

            // Second call: cache hit, no network.
            let r2 = fbuild_packages::lnk::resolve(&lnk, &cache).unwrap();
            assert_eq!(r2.sha256, sha);
            assert_eq!(r2.path, blob_path);
        },
    )
    .await;
}
