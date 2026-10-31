//! Tests for retry tuning, jitter and mirror fallback (FastLED/fbuild#1463).
//! Shares the local-server helpers of [`super::tests`].

use super::mirrors::{
    DEFAULT_MIRROR, MIRROR_FIRST_ENV, MIRRORS_ENV, attempt_order, candidate_urls,
    mirror_first_requested, resolve_candidates,
};
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
    let bytes = get_from_candidates(
        &test_client(),
        &urls,
        mirror_first_requested(),
        FAST_RETRY_TIMING,
    )
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
        mirror_first_requested(),
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
        mirror_first_requested(),
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
    get_from_candidates(
        &test_client(),
        &urls,
        mirror_first_requested(),
        FAST_RETRY_TIMING,
    )
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
    let err = get_from_candidates(
        &test_client(),
        &urls,
        mirror_first_requested(),
        FAST_RETRY_TIMING,
    )
    .await
    .expect_err("everything is down")
    .to_string();
    assert!(err.contains(&format!("127.0.0.1:{a}")), "{err}");
    assert!(
        err.contains("also tried 1 mirror(s)") && err.contains(&format!("127.0.0.1:{b}")),
        "{err}"
    );
}

/// `{path}` must reproduce the registry's own layout under the mirror, so a
/// bucket that mirrors `…/download/<owner>/<type>/<pkg>/<ver>/<file>` is hit
/// at exactly that key.
#[test]
fn candidate_urls_expand_path_for_a_registry_download_url() {
    let primary = "https://dl.registry.platformio.org/download/platformio/tool/framework-arduino-avr/5.4.0/framework-arduino-avr-5.4.0.tar.gz";
    assert_eq!(
        candidate_urls(
            primary,
            &["https://fbuild.sfo3.digitaloceanspaces.com/{path}".to_string()]
        ),
        vec![
            primary.to_string(),
            "https://fbuild.sfo3.digitaloceanspaces.com/platformio/tool/framework-arduino-avr/5.4.0/framework-arduino-avr-5.4.0.tar.gz".to_string(),
        ]
    );
}

/// Off the hosts the bucket mirrors there is no mirror path, so `{path}`
/// degrades to the file name, the same value `{filename}` produces.
#[test]
fn candidate_urls_path_falls_back_to_the_file_name_off_registry() {
    let mirrors = vec!["https://mirror.example/{path}".to_string()];
    assert_eq!(
        candidate_urls("https://vendor.example/files/ClearCore-1.7.4.zip", &mirrors)[1],
        "https://mirror.example/ClearCore-1.7.4.zip"
    );
    // A `/download/` segment on some other host is not a registry path.
    assert_eq!(
        candidate_urls(
            "https://downloads.example/download/v1.2.3/pkg.tar.gz",
            &mirrors
        )[1],
        "https://mirror.example/pkg.tar.gz"
    );
}

/// GitHub URLs keep owner and repo under a `github.com/` prefix, so two repos
/// shipping `1.8.6.tar.gz` never share a bucket key (FastLED/fbuild#1806).
#[test]
fn candidate_urls_path_keeps_github_owner_and_repo() {
    let mirrors = vec!["https://mirror.example/{path}".to_string()];
    assert_eq!(
        candidate_urls(
            "https://github.com/arduino/ArduinoCore-avr/archive/refs/tags/1.8.6.tar.gz",
            &mirrors
        )[1],
        "https://mirror.example/github.com/arduino/ArduinoCore-avr/archive/refs/tags/1.8.6.tar.gz"
    );
    assert_eq!(
        candidate_urls(
            "https://github.com/o/r/releases/download/v1.2.3/pkg.tar.gz",
            &mirrors
        )[1],
        "https://mirror.example/github.com/o/r/releases/download/v1.2.3/pkg.tar.gz"
    );
}

/// With no mirror configured, mirrored hosts go to fbuild's own bucket first
/// and fall back to the vendor; other hosts are left alone
/// (FastLED/fbuild#1806).
#[tokio::test]
async fn unset_env_prefers_the_built_in_bucket_for_mirrored_hosts() {
    let _guard = network_test_guard().await;
    let _mirrors = ScopedEnv::unset(MIRRORS_ENV);
    let _first = ScopedEnv::unset(MIRROR_FIRST_ENV);
    let bucket = DEFAULT_MIRROR.trim_end_matches("{path}");

    let registry = "https://dl.registry.platformio.org/download/platformio/platform/espressif32/6.5.0/espressif32-6.5.0.tar.gz";
    assert_eq!(
        resolve_candidates(registry),
        (
            vec![
                registry.to_string(),
                format!("{bucket}platformio/platform/espressif32/6.5.0/espressif32-6.5.0.tar.gz"),
            ],
            true
        )
    );

    let github = "https://github.com/o/r/archive/refs/tags/1.0.tar.gz";
    assert_eq!(
        resolve_candidates(github),
        (
            vec![
                github.to_string(),
                format!("{bucket}github.com/o/r/archive/refs/tags/1.0.tar.gz"),
            ],
            true
        )
    );

    let other = "https://vendor.example/files/pkg.zip";
    assert_eq!(resolve_candidates(other), (vec![other.to_string()], false));
}

/// `FBUILD_DOWNLOAD_MIRROR_FIRST=0` keeps the built-in bucket but puts the
/// vendor first; an explicitly empty `FBUILD_DOWNLOAD_MIRRORS` opts out.
#[tokio::test]
async fn env_overrides_the_built_in_bucket() {
    let _guard = network_test_guard().await;
    let registry = "https://dl.registry.platformio.org/download/o/tool/t/1.0/t-1.0.tar.gz";
    {
        let _mirrors = ScopedEnv::unset(MIRRORS_ENV);
        let _first = ScopedEnv::set(MIRROR_FIRST_ENV, "0");
        let (urls, mirror_first) = resolve_candidates(registry);
        assert_eq!(urls.len(), 2, "{urls:?}");
        assert!(!mirror_first);
    }
    let _mirrors = ScopedEnv::set(MIRRORS_ENV, "");
    let _first = ScopedEnv::unset(MIRROR_FIRST_ENV);
    assert_eq!(
        resolve_candidates(registry),
        (vec![registry.to_string()], false)
    );
}

/// `{filename}`, bare bases and `{path}` coexist, and the mirror order given
/// in the env var is the order tried.
#[test]
fn candidate_urls_keep_every_template_form_in_the_configured_order() {
    let primary = "https://dl.registry.platformio.org/download/platformio/platform/espressif32/6.5.0/espressif32-6.5.0.tar.gz";
    let mirrors = vec![
        "https://a.example/{path}".to_string(),
        "https://b.example/base/".to_string(),
        "https://c.example/{filename}?raw=1".to_string(),
        "https://d.example/{path}?name={filename}".to_string(),
    ];
    assert_eq!(
        candidate_urls(primary, &mirrors),
        vec![
            primary.to_string(),
            "https://a.example/platformio/platform/espressif32/6.5.0/espressif32-6.5.0.tar.gz"
                .to_string(),
            "https://b.example/base/espressif32-6.5.0.tar.gz".to_string(),
            "https://c.example/espressif32-6.5.0.tar.gz?raw=1".to_string(),
            // A template naming both leaves no literal placeholder behind.
            "https://d.example/platformio/platform/espressif32/6.5.0/espressif32-6.5.0.tar.gz?name=espressif32-6.5.0.tar.gz"
                .to_string(),
        ]
    );
}

#[test]
fn attempt_order_tries_the_primary_first_by_default() {
    assert_eq!(attempt_order(3, false), vec![0, 1, 2]);
    assert_eq!(attempt_order(1, false), vec![0]);
}

/// Mirror-first moves every mirror ahead of the primary, which stays the
/// last resort so a stale mirror can never be the only thing tried.
#[test]
fn attempt_order_tries_mirrors_first_when_requested() {
    assert_eq!(attempt_order(3, true), vec![1, 2, 0]);
    assert_eq!(
        attempt_order(1, true),
        vec![0],
        "no mirrors, nothing to swap"
    );
}

#[tokio::test]
async fn mirror_first_env_follows_the_truthy_convention() {
    let _guard = network_test_guard().await;
    for (value, expected) in [
        ("1", true),
        ("true", true),
        ("TRUE", true),
        ("yes", true),
        (" 1 ", true),
        ("on", true),
        (" ON ", true),
        ("0", false),
        ("", false),
        ("false", false),
        ("FALSE", false),
        ("no", false),
        ("off", false),
        ("2", false),
    ] {
        let _env = MirrorFirstEnv::set(value);
        assert_eq!(
            mirror_first_requested(),
            expected,
            "{MIRROR_FIRST_ENV}={value:?}"
        );
    }
    let _unset = MirrorFirstEnv::unset();
    assert!(!mirror_first_requested(), "unset must keep today's order");
}

/// Mirror-first: a healthy mirror serves the file and the primary is never
/// contacted. The unset case is covered by
/// [`mirrors_are_only_used_after_the_primary_fails`].
#[tokio::test]
async fn mirror_first_serves_from_the_mirror_without_touching_the_primary() {
    let _guard = network_test_guard().await;
    let _env = MirrorFirstEnv::set("1");
    let primary_hits = std::sync::Arc::new(AtomicUsize::new(0));
    let primary = run_constant_server(complete_response(), primary_hits.clone()).await;
    let mirror_hits = std::sync::Arc::new(AtomicUsize::new(0));
    let mirror = run_constant_server(complete_response(), mirror_hits.clone()).await;

    let urls = candidate_urls(
        &format!("http://127.0.0.1:{primary}/f.bin"),
        &[format!("http://127.0.0.1:{mirror}")],
    );
    let bytes = get_from_candidates(
        &test_client(),
        &urls,
        mirror_first_requested(),
        FAST_RETRY_TIMING,
    )
    .await
    .expect("the mirror should serve the file");

    assert_eq!(bytes, b"hello");
    assert_eq!(mirror_hits.load(Ordering::SeqCst), 1, "mirror goes first");
    assert_eq!(
        primary_hits.load(Ordering::SeqCst),
        0,
        "a healthy mirror must spare the primary"
    );
}

/// Even when the mirrors lead, the reported failure is still the primary
/// URL's own error with the mirrors named.
#[tokio::test]
async fn mirror_first_still_reports_the_primary_error_when_everything_is_down() {
    let _guard = network_test_guard().await;
    let _env = MirrorFirstEnv::set("1");
    let down = "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    let primary = run_constant_server(down, std::sync::Arc::new(AtomicUsize::new(0))).await;
    let mirror = run_constant_server(down, std::sync::Arc::new(AtomicUsize::new(0))).await;
    let urls = candidate_urls(
        &format!("http://127.0.0.1:{primary}/f.bin"),
        &[format!("http://127.0.0.1:{mirror}")],
    );

    let err = get_from_candidates(
        &test_client(),
        &urls,
        mirror_first_requested(),
        FAST_RETRY_TIMING,
    )
    .await
    .expect_err("everything is down")
    .to_string();

    assert!(err.contains(&format!("127.0.0.1:{primary}")), "{err}");
    assert!(
        err.contains("also tried 1 mirror(s)") && err.contains(&format!("127.0.0.1:{mirror}")),
        "{err}"
    );
}

/// Scoped override of any env var, restored on drop. Holders must also hold
/// [`network_test_guard`]: the env is process-global.
struct ScopedEnv(&'static str, Option<String>);

impl ScopedEnv {
    fn set(name: &'static str, value: &str) -> Self {
        let scoped = Self(name, std::env::var(name).ok());
        std::env::set_var(name, value);
        scoped
    }

    fn unset(name: &'static str) -> Self {
        let scoped = Self(name, std::env::var(name).ok());
        std::env::remove_var(name);
        scoped
    }
}

impl Drop for ScopedEnv {
    fn drop(&mut self) {
        match self.1.take() {
            Some(previous) => std::env::set_var(self.0, previous),
            None => std::env::remove_var(self.0),
        }
    }
}

/// Scoped `FBUILD_DOWNLOAD_MIRROR_FIRST` override, restored on drop. Holders
/// must also hold [`network_test_guard`]: the env is process-global.
struct MirrorFirstEnv(Option<String>);

impl MirrorFirstEnv {
    fn set(value: &str) -> Self {
        let scoped = Self(std::env::var(MIRROR_FIRST_ENV).ok());
        std::env::set_var(MIRROR_FIRST_ENV, value);
        scoped
    }

    fn unset() -> Self {
        let scoped = Self(std::env::var(MIRROR_FIRST_ENV).ok());
        std::env::remove_var(MIRROR_FIRST_ENV);
        scoped
    }
}

impl Drop for MirrorFirstEnv {
    fn drop(&mut self) {
        match self.0.take() {
            Some(previous) => std::env::set_var(MIRROR_FIRST_ENV, previous),
            None => std::env::remove_var(MIRROR_FIRST_ENV),
        }
    }
}
