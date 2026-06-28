//! Smoke test for the embedded zccache service. Originally added
//! for FastLED/fbuild#790; reframed in #800 once the embedded
//! backend became mandatory (the `embedded` Cargo feature was
//! deleted, the `embedded` cfg gate at the top of this file went
//! with it).

use fbuild_build::zccache_embedded::FbuildZccacheService;
use zccache::embedded::ShutdownMode;

/// `FbuildZccacheService::start_in` produces a working service
/// handle: the cache root exists on disk, the identity carries our
/// product tag, and a graceful shutdown returns cleanly.
///
/// Uses `tokio::test(flavor = "multi_thread")` to match the daemon's
/// runtime shape — `ZccacheService` spawns background tasks via
/// `tokio::spawn`, and a current-thread runtime would serialize
/// them behind the test future.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn embedded_service_starts_and_shuts_down() {
    // Per-test cache root so we don't collide with a running daemon
    // or with parallel test invocations against `~/.fbuild/`.
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let cache_root = tmp.path().join("zccache");

    let svc = FbuildZccacheService::start_in(cache_root.clone())
        .await
        .expect("embedded service should start cleanly under a fresh cache root");

    assert!(
        svc.cache_root().is_dir(),
        "cache root should exist on disk: {}",
        svc.cache_root().display()
    );
    assert_eq!(
        svc.cache_root(),
        cache_root.as_path(),
        "cache root should match the explicit path we passed in"
    );
    assert_eq!(svc.identity().product, "fbuild");

    svc.shutdown(ShutdownMode::Graceful)
        .await
        .expect("graceful shutdown should succeed");
}
