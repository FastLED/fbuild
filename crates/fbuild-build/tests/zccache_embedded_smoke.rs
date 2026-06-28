//! Phase 1 smoke test for FastLED/fbuild#790.
//!
//! Acceptance criterion: "starting the daemon with
//! `FBUILD_ZCCACHE_EMBEDDED=1` constructs the embedded service
//! without panicking, and the daemon logs which backend is active."
//!
//! This file covers the construct-without-panic half: we drive
//! [`FbuildZccacheService::start_in`] under the `embedded` feature
//! and assert it produces a working handle with a real on-disk cache
//! root. The daemon-startup log line is exercised by
//! `compile_backend::tests::name_is_wrapped_by_default` (the string
//! contract) and by the daemon's own startup path
//! (`crates/fbuild-daemon/src/main.rs`) at runtime.
//!
//! Gated `cfg(feature = "embedded")`: without the feature, the
//! `zccache_embedded` module + the `zccache` dep are not compiled
//! in. The test must not exist as a `#[test]` symbol when the
//! feature is off.

#![cfg(feature = "embedded")]

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
