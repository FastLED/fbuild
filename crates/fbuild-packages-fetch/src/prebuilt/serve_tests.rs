//! Ladder fixtures for the prebuilt serving decision (#1882): a real HTTP
//! server that can fail, real tar.zst bytes, and the real cache layout.
use super::*;
use crate::prebuilt::{MARKER, tree_root};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
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

/// A loopback server that fails the first `fail_first` requests with a 500 and
/// answers every later one with `body`; the counter counts every request.
async fn flaky_server(
    body: Vec<u8>,
    fail_first: usize,
) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let count = Arc::new(AtomicUsize::new(0));
    let calls = count.clone();
    let app = axum::Router::new().fallback(move || {
        let body = body.clone();
        let calls = calls.clone();
        async move {
            let seen = calls.fetch_add(1, Ordering::SeqCst);
            if seen < fail_first {
                return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Vec::new());
            }
            (axum::http::StatusCode::OK, body)
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

/// A transient source: two failures exhaust one fetch (resume, then fresh), so
/// the ladder has to retry to reach the bytes at all.
#[tokio::test]
async fn a_transient_failure_is_retried_once_and_then_served() {
    let archive = tar_zst("lib/libesp32.a", b"example");
    let (url, calls, task) = flaky_server(archive.clone(), 2).await;
    let temp = tempfile::tempdir().unwrap();
    let cache = temp.path().join("cache");
    let query = query(&"b".repeat(64));
    let asset = asset(&archive, vec![url]);
    let fetcher = Fetcher::new(&temp.path().join("downloads"), reqwest::Client::new());

    match serve(cache.as_path(), &query, &asset, &fetcher).await {
        Served::Prebuilt(obtained) => {
            assert!(obtained.downloaded, "the retry had to download the tree");
            assert_eq!(
                obtained.tree.root,
                tree_root(cache.as_path(), &query).unwrap()
            );
            assert_eq!(
                std::fs::read_to_string(obtained.tree.root.join("lib/libesp32.a").as_path())
                    .unwrap(),
                "example"
            );
        }
        Served::Source(fallback) => panic!("a transient source must be retried: {fallback:?}"),
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "one fetch exhausts two requests; the retry's first request is the third"
    );
    task.abort();
}

#[tokio::test]
async fn a_source_that_never_answers_falls_back_after_exactly_one_retry() {
    let archive = tar_zst("lib/libesp32.a", b"example");
    let (url, calls, task) = flaky_server(archive.clone(), usize::MAX).await;
    let temp = tempfile::tempdir().unwrap();
    let cache = temp.path().join("cache");
    let query = query(&"b".repeat(64));
    let asset = asset(&archive, vec![url]);
    let fetcher = Fetcher::new(&temp.path().join("downloads"), reqwest::Client::new());

    let served = serve(cache.as_path(), &query, &asset, &fetcher).await;
    let Served::Source(fallback) = served else {
        panic!("a dead source must fall back to source, got {served:?}");
    };
    assert_eq!(fallback.attempts, ATTEMPTS);
    assert!(fallback.reason.contains("attempt 1"), "{}", fallback.reason);
    assert!(fallback.reason.contains("attempt 2"), "{}", fallback.reason);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        4,
        "two requests per fetch, two fetches — never a third attempt"
    );
    assert!(
        !tree_root(cache.as_path(), &query)
            .unwrap()
            .as_path()
            .exists(),
        "a failed ladder must leave no tree behind"
    );
    task.abort();
}

/// The identity is wrong, so no attempt can succeed: prebuilt must not spend a
/// single request, and must not retry.
#[tokio::test]
async fn a_deterministic_identity_refusal_never_retries() {
    let archive = tar_zst("lib/libesp32.a", b"example");
    let (url, calls, task) = flaky_server(archive.clone(), 0).await;
    let temp = tempfile::tempdir().unwrap();
    let cache = temp.path().join("cache");
    let query = query(&"b".repeat(63));
    let asset = asset(&archive, vec![url]);
    let fetcher = Fetcher::new(&temp.path().join("downloads"), reqwest::Client::new());

    let served = serve(cache.as_path(), &query, &asset, &fetcher).await;
    let Served::Source(fallback) = served else {
        panic!("an invalid fingerprint must fall back, got {served:?}");
    };
    assert_eq!(fallback.attempts, 1, "{}", fallback.reason);
    assert!(
        fallback.reason.contains("64 lowercase hex"),
        "{}",
        fallback.reason
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    task.abort();
}

/// Another build's tree sits at the identity's path: retrying would fail the
/// same way and a download would be wasted, so fall back immediately.
#[tokio::test]
async fn a_foreign_tree_falls_back_without_download_or_retry() {
    let archive = tar_zst("lib/libesp32.a", b"example");
    let (url, calls, task) = flaky_server(archive.clone(), 0).await;
    let temp = tempfile::tempdir().unwrap();
    let cache = temp.path().join("cache");
    let query = query(&"b".repeat(64));
    let asset = asset(&archive, vec![url]);
    let fetcher = Fetcher::new(&temp.path().join("downloads"), reqwest::Client::new());

    let dest = tree_root(cache.as_path(), &query).unwrap();
    std::fs::create_dir_all(dest.as_path()).unwrap();
    std::fs::write(
        dest.join(MARKER).as_path(),
        json!({"sha256": "c".repeat(64), "size_bytes": 7, "entries": 1}).to_string(),
    )
    .unwrap();

    let served = serve(cache.as_path(), &query, &asset, &fetcher).await;
    let Served::Source(fallback) = served else {
        panic!("a conflicting tree must fall back, got {served:?}");
    };
    assert_eq!(fallback.attempts, 1, "{}", fallback.reason);
    assert!(
        fallback.reason.contains("left untouched"),
        "{}",
        fallback.reason
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "a refusal costs no download"
    );
    assert!(dest.join(MARKER).as_path().is_file(), "tree left untouched");
    task.abort();
}

#[tokio::test]
async fn a_complete_tree_is_served_without_a_request() {
    let archive = tar_zst("lib/libesp32.a", b"example");
    let (url, calls, task) = flaky_server(archive.clone(), 0).await;
    let temp = tempfile::tempdir().unwrap();
    let cache = temp.path().join("cache");
    let query = query(&"b".repeat(64));
    let asset = asset(&archive, vec![url]);
    let fetcher = Fetcher::new(&temp.path().join("downloads"), reqwest::Client::new());

    let first = crate::prebuilt::obtain(cache.as_path(), &query, &asset, &fetcher)
        .await
        .unwrap();
    let served = serve(cache.as_path(), &query, &asset, &fetcher).await;
    let Served::Prebuilt(obtained) = served else {
        panic!("a complete tree must be served, got {served:?}");
    };
    assert!(!obtained.downloaded);
    assert_eq!(obtained.tree, first.tree);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "the reuse stays offline");
    task.abort();
}

/// The rule, not just the examples above: every failure that a second attempt
/// could cure is retried, and nothing permanent is.
#[test]
fn the_retry_rule_admits_only_failures_a_second_attempt_could_cure() {
    let retried = [
        ObtainError::Fetch(FetchError::Transport {
            path: "https://example/x".into(),
            error: "connection reset".into(),
        }),
        ObtainError::Fetch(FetchError::Exhausted {
            last_error: "500".into(),
        }),
        ObtainError::Extract(ExtractError::Integrity {
            expected: "a".repeat(64),
            actual: "b".repeat(64),
            declared: 10,
            read: 10,
        }),
        ObtainError::Extract(ExtractError::Archive("truncated".into())),
        ObtainError::Extract(ExtractError::Io {
            path: "tree/x".into(),
            error: "no space left".into(),
        }),
        ObtainError::Extract(ExtractError::Join("cancelled".into())),
    ];
    for error in retried {
        assert!(worth_retrying(&error), "{error} must be retried once");
    }
    let permanent = [
        ObtainError::Fetch(FetchError::Invalid("sha256 must be 64 lowercase hex")),
        ObtainError::Fetch(FetchError::TooLarge {
            declared: 9,
            limit: 8,
        }),
        ObtainError::Extract(ExtractError::Unsupported("x.zip".into())),
        ObtainError::Extract(ExtractError::Entry("../x".into())),
        ObtainError::Extract(ExtractError::EntryType("x".into(), "symlink")),
        ObtainError::Extract(ExtractError::TooManyEntries { limit: 1 }),
        ObtainError::Extract(ExtractError::Occupied {
            path: "tree".into(),
        }),
        ObtainError::InvalidFingerprint("short".into()),
        ObtainError::InvalidComponent {
            field: "arch",
            value: "a/b".into(),
        },
        ObtainError::IdentityConflict {
            path: "tree".into(),
            existing: None,
            expected: "a".repeat(64),
        },
    ];
    for error in permanent {
        assert!(!worth_retrying(&error), "{error} must not be retried");
    }
}

#[test]
fn the_notice_names_every_attempt() {
    let fallback = Fallback {
        reason: "attempt 1: x; attempt 2: y".into(),
        attempts: 2,
    };
    let notice = fallback.notice();
    assert!(notice.contains("2 attempt(s)"), "{notice}");
    assert!(notice.contains("from source"), "{notice}");
    assert!(notice.contains("attempt 1: x"), "{notice}");
    assert!(notice.contains("attempt 2: y"), "{notice}");
}
