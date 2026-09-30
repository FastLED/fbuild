//! Tests for retry tuning, jitter and mirror fallback (FastLED/fbuild#1463).
//! Shares the local-server helpers of [`super::tests`].

use super::tests::{
    FAST_RETRY_TIMING, complete_response, network_test_guard, run_flaky_server, test_client,
};
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn env_overrides_change_attempts_and_wait_and_ignore_garbage() {
    let base = RetryTiming::PRODUCTION;
    let t = base.with_overrides(Some("8"), Some("20"));
    assert_eq!(t.max_attempts, 8);
    assert_eq!(t.max_wait, Duration::from_secs(20));
    // Waits are capped, and attempts past the schedule repeat its last entry.
    assert_eq!(t.backoff(4), Duration::from_secs(20));
    assert_eq!(t.backoff(7), Duration::from_secs(20));
    assert_eq!(t.backoff(1), Duration::from_secs(5));
    // Out-of-range clamps; unparsable keeps the default.
    assert_eq!(base.with_overrides(Some("0"), None).max_attempts, 1);
    assert_eq!(base.with_overrides(Some("999"), None).max_attempts, 20);
    let kept = base.with_overrides(Some("many"), Some("soon"));
    assert_eq!(kept.max_attempts, MAX_ATTEMPTS);
    assert_eq!(kept.max_wait, DEFAULT_MAX_WAIT);
}

#[test]
fn jitter_stays_within_half_to_full_delay() {
    let delay = Duration::from_secs(30);
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..200 {
        let j = jittered(delay);
        assert!(j >= delay / 2 && j <= delay, "{j:?}");
        seen.insert(j.as_nanos());
    }
    assert!(seen.len() > 1, "jitter produced a constant");
}

#[test]
fn candidate_urls_put_the_primary_first_and_keep_the_file_name() {
    let mirrors = vec![
        "https://mirror.example/pinned/".to_string(),
        "https://cdn.example/{filename}?raw=1".to_string(),
    ];
    assert_eq!(
        candidate_urls("https://vendor.example/files/ClearCore-1.7.4.zip", &mirrors),
        vec![
            "https://vendor.example/files/ClearCore-1.7.4.zip".to_string(),
            "https://mirror.example/pinned/ClearCore-1.7.4.zip".to_string(),
            "https://cdn.example/ClearCore-1.7.4.zip?raw=1".to_string(),
        ]
    );
    assert_eq!(
        candidate_urls("https://a/b.zip", &[]),
        vec!["https://a/b.zip"]
    );
}

/// A server that answers every request with `response`, counting requests.
async fn run_constant_server(response: &'static str, count: std::sync::Arc<AtomicUsize>) -> u16 {
    let responses = std::sync::Arc::new(std::sync::Mutex::new(vec![response; 64]));
    run_flaky_server(responses, count).await
}

/// Primary host down for its whole budget, then the mirror serves the pinned
/// archive and its SHA-256 verifies (FastLED/fbuild#1463).
#[tokio::test]
async fn buffered_download_falls_back_to_a_mirror_when_the_primary_is_down() {
    let _guard = network_test_guard().await;
    let down = "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    let primary_hits = std::sync::Arc::new(AtomicUsize::new(0));
    let primary = run_constant_server(down, primary_hits.clone()).await;
    let mirror_hits = std::sync::Arc::new(AtomicUsize::new(0));
    let mirror = run_constant_server(complete_response(), mirror_hits.clone()).await;

    let urls = candidate_urls(
        &format!("http://127.0.0.1:{primary}/files/pkg.zip"),
        &[format!("http://127.0.0.1:{mirror}/pinned")],
    );
    let bytes = get_from_candidates(&test_client(), &urls, FAST_RETRY_TIMING)
        .await
        .expect("the mirror should serve the file");

    assert_eq!(bytes, b"hello");
    assert_eq!(
        primary_hits.load(Ordering::SeqCst),
        5,
        "primary budget spent first"
    );
    assert_eq!(mirror_hits.load(Ordering::SeqCst), 1);
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("pkg.zip");
    std::fs::write(&path, &bytes).unwrap();
    // sha256("hello")
    verify_checksum(
        &path,
        "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
    )
    .expect("mirror bytes match the pinned SHA-256");
}

#[tokio::test]
async fn streaming_download_falls_back_to_a_mirror_when_the_primary_is_down() {
    let _guard = network_test_guard().await;
    let down = "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    let primary = run_constant_server(down, std::sync::Arc::new(AtomicUsize::new(0))).await;
    let mirror = run_constant_server(
        complete_response(),
        std::sync::Arc::new(AtomicUsize::new(0)),
    )
    .await;
    let urls = candidate_urls(
        &format!("http://127.0.0.1:{primary}/pkg.bin"),
        &[format!("http://127.0.0.1:{mirror}")],
    );
    let temp = tempfile::TempDir::new().unwrap();
    let mut progress = |_p: &DownloadProgress| {};

    download_from_candidates(
        &test_client(),
        &urls,
        temp.path(),
        &mut progress,
        FAST_RETRY_TIMING,
    )
    .await
    .expect("the mirror should serve the file");

    assert_eq!(
        std::fs::read(temp.path().join("pkg.bin")).unwrap(),
        b"hello"
    );
}

/// A mirror template with a query string must not leak into the file name:
/// the archive has to land under the primary's name, where the caller looks.
#[tokio::test]
async fn streaming_mirror_with_a_query_string_keeps_the_primary_file_name() {
    let _guard = network_test_guard().await;
    let down = "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    let primary = run_constant_server(down, std::sync::Arc::new(AtomicUsize::new(0))).await;
    let mirror = run_constant_server(
        complete_response(),
        std::sync::Arc::new(AtomicUsize::new(0)),
    )
    .await;
    let urls = candidate_urls(
        &format!("http://127.0.0.1:{primary}/pkg.bin"),
        &[format!("http://127.0.0.1:{mirror}/{{filename}}?raw=1")],
    );
    assert!(urls[1].ends_with("/pkg.bin?raw=1"), "{urls:?}");
    let temp = tempfile::TempDir::new().unwrap();
    let mut progress = |_p: &DownloadProgress| {};

    download_from_candidates(
        &test_client(),
        &urls,
        temp.path(),
        &mut progress,
        FAST_RETRY_TIMING,
    )
    .await
    .expect("the mirror should serve the file");

    assert_eq!(
        std::fs::read(temp.path().join("pkg.bin")).unwrap(),
        b"hello"
    );
    assert!(!temp.path().join("pkg.bin?raw=1").exists());
}

/// The primary being fine must never touch a mirror, and when everything is
/// down the error is the primary's, with the mirrors named.
#[tokio::test]
async fn mirrors_are_only_used_after_the_primary_fails() {
    let _guard = network_test_guard().await;
    let mirror_hits = std::sync::Arc::new(AtomicUsize::new(0));
    let mirror = run_constant_server(complete_response(), mirror_hits.clone()).await;
    let primary = run_constant_server(
        complete_response(),
        std::sync::Arc::new(AtomicUsize::new(0)),
    )
    .await;
    let urls = candidate_urls(
        &format!("http://127.0.0.1:{primary}/f.bin"),
        &[format!("http://127.0.0.1:{mirror}")],
    );
    get_from_candidates(&test_client(), &urls, FAST_RETRY_TIMING)
        .await
        .unwrap();
    assert_eq!(mirror_hits.load(Ordering::SeqCst), 0);

    let down = "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    let a = run_constant_server(down, std::sync::Arc::new(AtomicUsize::new(0))).await;
    let b = run_constant_server(down, std::sync::Arc::new(AtomicUsize::new(0))).await;
    let urls = candidate_urls(
        &format!("http://127.0.0.1:{a}/f.bin"),
        &[format!("http://127.0.0.1:{b}")],
    );
    let err = get_from_candidates(&test_client(), &urls, FAST_RETRY_TIMING)
        .await
        .expect_err("everything is down")
        .to_string();
    assert!(err.contains(&format!("127.0.0.1:{a}")), "{err}");
    assert!(
        err.contains("also tried 1 mirror(s)") && err.contains(&format!("127.0.0.1:{b}")),
        "{err}"
    );
}
