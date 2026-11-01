//! Adversarial tests for the server-side mirror manifest (FastLED/fbuild#1810).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use super::DownloadProgress;
use super::manifest::{
    CACHE_TTL, MAX_MANIFEST_BYTES, MirrorManifest, cache_path, is_allowed_url, is_loopback_url,
    load_with,
};
use super::mirror_tests::ScopedEnv;
use super::mirrors::{
    Candidates, MIRROR_FIRST_ENV, MIRRORS_ENV, download_from_candidates, get_from_candidates,
    resolve_with,
};
use super::tests::{FAST_RETRY_TIMING, network_test_guard, run_flaky_server, test_client};

const VENDOR: &str = "https://dl.registry.platformio.org/download/o/tool/t/1.0/t-1.0.tar.gz";
const BUCKET: &str = "https://fbuild.sfo3.digitaloceanspaces.com/o/tool/t/1.0/t-1.0.tar.gz";
const OTHER: &str = "https://other.example/t-1.0.tar.gz";
/// sha256("hello")
const HELLO_SHA: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

fn doc(components: &str) -> String {
    format!(
        r#"{{"$schema":"https://zackees.github.io/manifest.json/v1/manifest.schema.json",
            "kind":"Release","schema_version":1,"tool":"fbuild-download-mirror",
            "version":"2026.10.31","components":[{components}]}}"#
    )
}

fn component(urls: &[&str], extra: &str) -> String {
    let urls: Vec<String> = urls.iter().map(|u| format!("{u:?}")).collect();
    format!(
        r#"{{"id":"t","required":false,"asset":{{"filename":"t-1.0.tar.gz","urls":[{}]{extra}}}}}"#,
        urls.join(",")
    )
}

fn parse(json: &str) -> MirrorManifest {
    MirrorManifest::parse(json.as_bytes()).expect("valid manifest")
}

fn rejects(json: &str, needle: &str) {
    let err = MirrorManifest::parse(json.as_bytes()).expect_err("must be rejected");
    assert!(
        err.contains(needle),
        "error {err:?} should mention {needle:?}"
    );
}

fn http_ok(body: &str) -> &'static str {
    Box::leak(
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_boxed_str(),
    )
}

const DOWN: &str =
    "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

async fn server(responses: Vec<&'static str>) -> (String, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let port = run_flaky_server(Arc::new(std::sync::Mutex::new(responses)), hits.clone()).await;
    (format!("http://127.0.0.1:{port}/manifest.json"), hits)
}

// ---------------------------------------------------------------- parsing

#[test]
fn valid_manifest_is_indexed_by_every_listed_url() {
    let m = parse(&doc(&component(
        &[BUCKET, VENDOR],
        &format!(r#","sha256":"{HELLO_SHA}""#),
    )));
    for url in [BUCKET, VENDOR] {
        let entry = m.lookup(url).expect("listed");
        assert_eq!(entry.urls, vec![BUCKET, VENDOR]);
        assert_eq!(entry.sha256.as_deref(), Some(HELLO_SHA));
        assert!(!entry.yanked);
    }
    assert!(m.lookup(OTHER).is_none());
}

#[test]
fn unknown_fields_and_assetless_components_are_tolerated() {
    let m = parse(&doc(&format!(
        r#"{{"id":"no-asset"}},{},{{"id":"x","future_field":[1,2]}}"#,
        component(
            &[VENDOR],
            r#","media_type":"application/gzip","size_bytes":5,"provides":[]"#
        )
    )));
    assert!(m.lookup(VENDOR).is_some());
}

#[test]
fn empty_components_is_a_valid_empty_manifest() {
    assert!(parse(&doc("")).lookup(VENDOR).is_none());
}

#[test]
fn wrong_kind_or_schema_version_is_rejected() {
    rejects(
        &doc("").replace(r#""kind":"Release""#, r#""kind":"Catalog""#),
        "kind",
    );
    rejects(
        &doc("").replace(r#""schema_version":1"#, r#""schema_version":2"#),
        "schema_version",
    );
    rejects(r#"{"schema_version":1}"#, "not valid");
}

#[test]
fn garbage_bodies_are_rejected() {
    for body in ["", "null", "[]", "<html>404</html>", "{", "\u{feff}{}"] {
        assert!(MirrorManifest::parse(body.as_bytes()).is_err(), "{body:?}");
    }
}

#[test]
fn oversized_body_is_rejected_before_parsing() {
    let mut body = doc("").into_bytes();
    body.resize(MAX_MANIFEST_BYTES + 1, b' ');
    assert!(MirrorManifest::parse(&body).unwrap_err().contains("limit"));
}

#[test]
fn unsafe_urls_reject_the_whole_document() {
    for bad in [
        "http://evil.example/t.tar.gz",
        "ftp://evil.example/t.tar.gz",
        "file:///etc/passwd",
        "https://user:pass@evil.example/t.tar.gz",
        "http://127.0.0.1.evil.example/t.tar.gz",
        "http://localhost@evil.example/t.tar.gz",
        "https:///no-host",
        "https://evil.example/a b",
        "https://evil.example/a\u{8}b",
        "https://evil.example/a\u{0}b",
        "/relative/path.tar.gz",
        "",
    ] {
        // A good entry alongside must not rescue a document with a bad one.
        let json = doc(&format!(
            "{},{}",
            component(&[VENDOR], ""),
            component(&[OTHER, bad], "")
        ));
        assert!(
            MirrorManifest::parse(json.as_bytes()).is_err(),
            "{bad:?} must reject the document"
        );
    }
}

#[test]
fn malformed_assets_are_rejected() {
    rejects(&doc(&component(&[], "")), "no urls");
    rejects(&doc(&component(&[VENDOR, VENDOR], "")), "twice");
    rejects(
        &doc(&format!(
            "{},{}",
            component(&[VENDOR], ""),
            component(&[BUCKET, VENDOR], "")
        )),
        "twice",
    );
    for sha in [
        "abc",
        &HELLO_SHA.to_uppercase(),
        &format!("{HELLO_SHA}00"),
        &HELLO_SHA.replace('2', "g"),
    ] {
        rejects(
            &doc(&component(&[VENDOR], &format!(r#","sha256":"{sha}""#))),
            "sha256",
        );
    }
    rejects(
        &doc(&component(&[VENDOR], r#","yanked":"yes""#)),
        "not valid",
    );
}

#[test]
fn url_policy_allows_https_and_loopback_http_only() {
    for ok in [
        "https://fastled.github.io/fbuild/mirror/manifest.json",
        "http://127.0.0.1:8080/m.json",
        "http://localhost/m.json",
        "http://[::1]:9/m.json",
    ] {
        assert!(is_allowed_url(ok), "{ok}");
    }
    assert!(is_loopback_url("http://127.0.0.1:1/x"));
    assert!(!is_loopback_url("https://127.0.0.1.evil.example/x"));
    assert!(!is_loopback_url(VENDOR));
}

// -------------------------------------------------------------- resolving

#[tokio::test]
async fn manifest_order_decides_the_attempt_order() {
    let _guard = network_test_guard().await;
    let _m = ScopedEnv::unset(MIRRORS_ENV);
    let _f = ScopedEnv::unset(MIRROR_FIRST_ENV);
    let m = parse(&doc(&component(
        &[BUCKET, VENDOR, OTHER],
        &format!(r#","sha256":"{HELLO_SHA}""#),
    )));

    let c = resolve_with(VENDOR, m.lookup(VENDOR));
    assert_eq!(
        c.urls,
        vec![VENDOR, BUCKET, OTHER],
        "primary stays at index 0"
    );
    assert_eq!(c.order, vec![1, 0, 2], "bucket, then vendor, then other");
    assert_eq!(c.mirror_sha256.as_deref(), Some(HELLO_SHA));

    // Looked up by a mirror URL, that URL is the primary.
    let c = resolve_with(BUCKET, m.lookup(BUCKET));
    assert_eq!(c.urls, vec![BUCKET, VENDOR, OTHER]);
    assert_eq!(c.order, vec![0, 1, 2]);
}

#[tokio::test]
async fn yanked_entries_go_straight_to_the_primary() {
    let _guard = network_test_guard().await;
    let _m = ScopedEnv::unset(MIRRORS_ENV);
    let m = parse(&doc(&component(&[BUCKET, VENDOR], r#","yanked":true"#)));
    assert_eq!(
        resolve_with(VENDOR, m.lookup(VENDOR)),
        Candidates::new(vec![VENDOR.to_string()], false)
    );
}

#[tokio::test]
async fn operator_env_beats_the_manifest() {
    let _guard = network_test_guard().await;
    let m = parse(&doc(&component(&[BUCKET, VENDOR], "")));
    {
        let _m = ScopedEnv::set(MIRRORS_ENV, "");
        let _f = ScopedEnv::unset(MIRROR_FIRST_ENV);
        assert_eq!(
            resolve_with(VENDOR, m.lookup(VENDOR)),
            Candidates::new(vec![VENDOR.to_string()], false),
            "an explicit (empty) mirror list ignores the manifest"
        );
    }
    let _m = ScopedEnv::unset(MIRRORS_ENV);
    let _f = ScopedEnv::set(MIRROR_FIRST_ENV, "0");
    let c = resolve_with(VENDOR, m.lookup(VENDOR));
    assert_eq!(c.urls, vec![VENDOR, BUCKET]);
    assert_eq!(c.order, vec![0, 1], "MIRROR_FIRST=0 puts the vendor first");
}

#[tokio::test]
async fn unlisted_urls_keep_the_built_in_default() {
    let _guard = network_test_guard().await;
    let _m = ScopedEnv::unset(MIRRORS_ENV);
    let _f = ScopedEnv::unset(MIRROR_FIRST_ENV);
    let m = parse(&doc(&component(&[OTHER], "")));
    assert_eq!(
        resolve_with(VENDOR, m.lookup(VENDOR)),
        resolve_with(VENDOR, None)
    );
}

// ---------------------------------------------------------------- loading

#[tokio::test]
async fn fetched_manifest_is_cached_and_reused_while_fresh() {
    let _guard = network_test_guard().await;
    let dir = tempfile::TempDir::new().unwrap();
    let cache = dir.path().join("m.json");
    let body = doc(&component(&[BUCKET, VENDOR], ""));
    let (url, hits) = server(vec![http_ok(&body)]).await;

    let m = load_with(
        &test_client(),
        std::slice::from_ref(&url),
        Some(&cache),
        CACHE_TTL,
    )
    .await;
    assert!(m.unwrap().lookup(VENDOR).is_some());
    assert_eq!(std::fs::read_to_string(&cache).unwrap(), body);

    // The server has nothing left to serve; a fresh cache must not ask.
    let m = load_with(&test_client(), &[url], Some(&cache), CACHE_TTL).await;
    assert!(m.unwrap().lookup(VENDOR).is_some());
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn stale_cache_survives_a_down_or_hostile_server() {
    let _guard = network_test_guard().await;
    let dir = tempfile::TempDir::new().unwrap();
    let cache = dir.path().join("m.json");
    let good = doc(&component(&[BUCKET, VENDOR], ""));
    std::fs::write(&cache, &good).unwrap();

    let hostile = doc(&component(&["http://evil.example/x"], ""));
    for responses in [
        vec![DOWN],
        vec![http_ok("<html>not json</html>")],
        vec![http_ok(&hostile)],
    ] {
        let (url, hits) = server(responses).await;
        let m = load_with(&test_client(), &[url], Some(&cache), Duration::ZERO).await;
        assert!(m.expect("stale cache").lookup(VENDOR).is_some());
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "a stale cache asks the server"
        );
        assert_eq!(
            std::fs::read_to_string(&cache).unwrap(),
            good,
            "a rejected body never overwrites the cache"
        );
    }
}

#[tokio::test]
async fn no_cache_and_no_server_means_no_manifest() {
    let _guard = network_test_guard().await;
    let dir = tempfile::TempDir::new().unwrap();
    let (url, _) = server(vec![DOWN]).await;
    let cache = dir.path().join("m.json");
    assert!(
        load_with(&test_client(), &[url], Some(&cache), CACHE_TTL)
            .await
            .is_none()
    );
    assert!(!cache.exists());
}

#[tokio::test]
async fn second_source_serves_when_the_first_fails() {
    let _guard = network_test_guard().await;
    let (first, first_hits) = server(vec![DOWN]).await;
    let (second, _) = server(vec![http_ok(&doc(&component(&[VENDOR], "")))]).await;
    let m = load_with(&test_client(), &[first, second], None, CACHE_TTL).await;
    assert!(m.unwrap().lookup(VENDOR).is_some());
    assert_eq!(first_hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn corrupt_cache_is_replaced_by_a_good_fetch() {
    let _guard = network_test_guard().await;
    let dir = tempfile::TempDir::new().unwrap();
    let cache = dir.path().join("m.json");
    std::fs::write(&cache, "{ truncated").unwrap();
    let body = doc(&component(&[VENDOR], ""));
    let (url, _) = server(vec![http_ok(&body)]).await;
    let m = load_with(&test_client(), &[url], Some(&cache), CACHE_TTL).await;
    assert!(m.unwrap().lookup(VENDOR).is_some());
    assert_eq!(std::fs::read_to_string(&cache).unwrap(), body);
}

#[tokio::test]
async fn non_https_sources_are_never_requested() {
    let _guard = network_test_guard().await;
    let m = load_with(
        &test_client(),
        &["http://evil.example/manifest.json".to_string()],
        None,
        CACHE_TTL,
    )
    .await;
    assert!(m.is_none());
}

#[tokio::test]
async fn oversized_response_is_rejected_while_streaming() {
    let _guard = network_test_guard().await;
    let mut body = doc("");
    body.push_str(&" ".repeat(MAX_MANIFEST_BYTES));
    let (url, _) = server(vec![http_ok(&body)]).await;
    assert!(
        load_with(&test_client(), &[url], None, CACHE_TTL)
            .await
            .is_none()
    );
}

#[test]
fn cache_files_are_keyed_by_source_list() {
    let root = std::path::Path::new("/cache");
    let a = cache_path(root, &["https://a/m.json".to_string()]);
    let b = cache_path(root, &["https://b/m.json".to_string()]);
    assert_ne!(a, b);
    assert_eq!(a, cache_path(root, &["https://a/m.json".to_string()]));
    let dir = fbuild_core::path::NormalizedPath::new(root).join("mirror-manifest");
    assert!(a.relative_to(&dir).is_some(), "{a:?} not under {dir:?}");
}

// ------------------------------------------------------------ downloading

/// Candidates whose mirror (tried first) and primary are local servers.
async fn mirror_then_primary(
    mirror_body: &'static str,
    primary_body: &'static str,
    sha: &str,
) -> (Candidates, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let (mirror, mirror_hits) = server(vec![http_ok(mirror_body); 4]).await;
    let (primary, primary_hits) = server(vec![http_ok(primary_body); 4]).await;
    let primary = primary.replace("manifest.json", "t-1.0.tar.gz");
    let mirror = mirror.replace("manifest.json", "t-1.0.tar.gz");
    let candidates = Candidates {
        urls: vec![primary, mirror],
        order: vec![1, 0],
        mirror_sha256: Some(sha.to_string()),
    };
    (candidates, mirror_hits, primary_hits)
}

#[tokio::test]
async fn buffered_mirror_with_wrong_bytes_falls_through_to_the_primary() {
    let _guard = network_test_guard().await;
    let (c, mirror_hits, primary_hits) = mirror_then_primary("EVIL!", "hello", HELLO_SHA).await;
    let bytes = get_from_candidates(&test_client(), &c, FAST_RETRY_TIMING)
        .await
        .unwrap();
    assert_eq!(bytes, b"hello");
    assert_eq!(mirror_hits.load(Ordering::SeqCst), 1);
    assert_eq!(primary_hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn streaming_mirror_with_wrong_bytes_falls_through_to_the_primary() {
    let _guard = network_test_guard().await;
    let (c, _, primary_hits) = mirror_then_primary("EVIL!", "hello", HELLO_SHA).await;
    let dir = tempfile::TempDir::new().unwrap();
    let mut progress = |_p: &DownloadProgress| {};
    download_from_candidates(
        &test_client(),
        &c,
        dir.path(),
        &mut progress,
        FAST_RETRY_TIMING,
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read(dir.path().join("t-1.0.tar.gz")).unwrap(),
        b"hello"
    );
    assert_eq!(primary_hits.load(Ordering::SeqCst), 1);
    assert!(!dir.path().join("t-1.0.tar.gz.part").exists());
}

#[tokio::test]
async fn mirror_with_matching_bytes_spares_the_primary() {
    let _guard = network_test_guard().await;
    let (c, _, primary_hits) = mirror_then_primary("hello", "hello", HELLO_SHA).await;
    let bytes = get_from_candidates(&test_client(), &c, FAST_RETRY_TIMING)
        .await
        .unwrap();
    assert_eq!(bytes, b"hello");
    assert_eq!(primary_hits.load(Ordering::SeqCst), 0);
}

/// #1815: a download costs the vendor exactly one request, the download
/// itself; no HEAD for a checksum and no `?bypass=` retry for a next mirror.
#[tokio::test]
async fn vendor_sees_exactly_one_request_per_download() {
    let _guard = network_test_guard().await;
    for mirror_body in ["EVIL!", "hello"] {
        let (mut c, _, primary_hits) = mirror_then_primary(mirror_body, "hello", HELLO_SHA).await;
        let expected = usize::from(mirror_body != "hello");
        get_from_candidates(&test_client(), &c, FAST_RETRY_TIMING)
            .await
            .unwrap();
        assert_eq!(
            primary_hits.load(Ordering::SeqCst),
            expected,
            "{mirror_body}"
        );
        c.order = vec![0];
        get_from_candidates(&test_client(), &c, FAST_RETRY_TIMING)
            .await
            .unwrap();
        assert_eq!(
            primary_hits.load(Ordering::SeqCst),
            expected + 1,
            "primary only"
        );
    }
}

/// A wrong sha in the manifest must only disqualify mirrors: the vendor's
/// bytes are the caller's to verify, so a bad manifest cannot break builds.
#[tokio::test]
async fn wrong_manifest_sha_never_blocks_the_primary() {
    let _guard = network_test_guard().await;
    let wrong = "0".repeat(64);
    let (c, _, _) = mirror_then_primary("hello", "hello", &wrong).await;
    let bytes = get_from_candidates(&test_client(), &c, FAST_RETRY_TIMING)
        .await
        .unwrap();
    assert_eq!(bytes, b"hello");
}

/// Every candidate bad: the error is still the primary's own.
#[tokio::test]
async fn all_mirrors_rejected_and_primary_down_reports_the_primary() {
    let _guard = network_test_guard().await;
    let (mirror, _) = server(vec![http_ok("EVIL!")]).await;
    let (primary, _) = server(vec![DOWN; 5]).await;
    let c = Candidates {
        urls: vec![primary.clone(), mirror],
        order: vec![1, 0],
        mirror_sha256: Some(HELLO_SHA.to_string()),
    };
    let err = get_from_candidates(&test_client(), &c, FAST_RETRY_TIMING)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains(&primary), "{err}");
}

// ------------------------------------------------------------------ shards

fn sha_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn index(owner: &str, url: &str, shard: &str) -> String {
    format!(
        r#"{{"kind":"Index","schema_version":1,"tools":{{"{owner}":{{"descriptor":{{"url":"{url}","sha256":"{}","size_bytes":{}}}}}}}}}"#,
        sha_hex(shard.as_bytes()),
        shard.len()
    )
}

/// An Index served at `/manifest.json` with `owner`'s shard at
/// `/shards/o.json`; `shard_responses` is what the shard URL serves, in order.
async fn sharded(
    owner: &str,
    shard: &str,
    shard_responses: Vec<&'static str>,
) -> (Vec<String>, Arc<AtomicUsize>) {
    let (shard_url, shard_hits) = server(shard_responses).await;
    // A relative descriptor resolves against the manifest's own directory,
    // so serve the index from the shard server's port.
    let base = shard_url.replace("manifest.json", "");
    let idx = index(owner, &format!("{base}shards/o.json"), shard);
    let (idx_url, _) = server(vec![http_ok(&idx)]).await;
    (vec![idx_url], shard_hits)
}

#[test]
fn index_descriptors_are_validated() {
    let shard = doc(&component(&[BUCKET, VENDOR], ""));
    let ok = MirrorManifest::parse(index("o", "shards/o.json", &shard).as_bytes()).unwrap();
    assert_eq!(ok.shard_ref("o").unwrap().url, "shards/o.json");
    for bad in [
        "../escape.json",
        "/abs.json",
        "shards/../../x.json",
        "http://evil.example/s.json",
        "shards/o.json?x=1",
        "a\\b.json",
        "",
    ] {
        let json = index("o", bad, &shard);
        assert!(
            MirrorManifest::parse(json.as_bytes()).is_err(),
            "{bad:?} must be rejected"
        );
    }
    for json in [
        index("", "s.json", &shard),
        index("a/b", "s.json", &shard),
        index("o", "s.json", &shard).replace(&sha_hex(shard.as_bytes()), "abc"),
        index("o", "s.json", &shard).replace(
            &format!("\"size_bytes\":{}", shard.len()),
            "\"size_bytes\":99999999999",
        ),
        r#"{"kind":"Index","schema_version":1,"tools":{"o":{"summary":"no descriptor"}}}"#
            .to_string(),
    ] {
        assert!(MirrorManifest::parse(json.as_bytes()).is_err(), "{json}");
    }
}

#[tokio::test]
async fn shard_is_fetched_verified_and_used() {
    let _guard = network_test_guard().await;
    let shard = doc(&component(
        &[BUCKET, VENDOR],
        &format!(r#","sha256":"{HELLO_SHA}""#),
    ));
    let (sources, hits) = sharded("o", &shard, vec![http_ok(&shard)]).await;
    let dir = tempfile::TempDir::new().unwrap();
    let m = load_with(
        &test_client(),
        &sources,
        Some(&dir.path().join("m.json")),
        CACHE_TTL,
    )
    .await
    .unwrap();
    assert!(
        m.lookup(VENDOR).is_none(),
        "the index itself lists no assets"
    );
    let entry = m.entry_for(VENDOR, Some("o")).await.expect("shard entry");
    assert_eq!(entry.sha256.as_deref(), Some(HELLO_SHA));
    // Second lookup, other owner: no extra fetches.
    assert!(m.entry_for(VENDOR, Some("o")).await.is_some());
    assert!(m.entry_for(VENDOR, Some("other")).await.is_none());
    assert!(m.entry_for(VENDOR, None).await.is_none());
    assert_eq!(hits.load(Ordering::SeqCst), 1, "one shard fetch per owner");
}

#[tokio::test]
async fn tampered_or_hostile_shards_are_rejected() {
    let _guard = network_test_guard().await;
    let shard = doc(&component(&[BUCKET, VENDOR], ""));
    let nested = index("o", "x.json", &shard);
    for served in [
        // Bytes that don't hash to the descriptor.
        http_ok(&doc(&component(&[OTHER, VENDOR], ""))),
        DOWN,
        http_ok("<html>"),
    ] {
        let (sources, _) = sharded("o", &shard, vec![served]).await;
        let m = load_with(&test_client(), &sources, None, CACHE_TTL)
            .await
            .unwrap();
        assert!(m.entry_for(VENDOR, Some("o")).await.is_none());
    }
    // A shard that is itself an Index is refused even when its hash matches.
    let (sources, _) = sharded("o", &nested, vec![http_ok(&nested)]).await;
    let m = load_with(&test_client(), &sources, None, CACHE_TTL)
        .await
        .unwrap();
    assert!(m.entry_for(VENDOR, Some("o")).await.is_none());
}

#[tokio::test]
async fn cached_shard_is_reused_and_survives_an_outage() {
    let _guard = network_test_guard().await;
    let dir = tempfile::TempDir::new().unwrap();
    let cache = dir.path().join("m.json");
    let shard = doc(&component(&[BUCKET, VENDOR], ""));
    let (sources, hits) = sharded("o", &shard, vec![http_ok(&shard)]).await;
    let m = load_with(&test_client(), &sources, Some(&cache), CACHE_TTL)
        .await
        .unwrap();
    assert!(m.entry_for(VENDOR, Some("o")).await.is_some());

    // New process (fresh manifest from cache): the verified shard cache is
    // used without asking the server again.
    let m = load_with(&test_client(), &sources, Some(&cache), CACHE_TTL)
        .await
        .unwrap();
    assert!(m.entry_for(VENDOR, Some("o")).await.is_some());
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    // The index moves on to a shard nobody can serve: the stale cached
    // shard keeps working.
    let newer = doc(&component(&[BUCKET, VENDOR, OTHER], ""));
    let (sources, _) = sharded("o", &newer, vec![DOWN]).await;
    let m = load_with(
        &test_client(),
        &sources,
        Some(&dir.path().join("m2.json")),
        CACHE_TTL,
    )
    .await
    .unwrap();
    assert!(
        m.entry_for(VENDOR, Some("o")).await.is_some(),
        "stale shard from the shared shard cache"
    );
}

#[tokio::test]
async fn relative_shard_urls_resolve_against_the_manifest_source() {
    let _guard = network_test_guard().await;
    let shard = doc(&component(&[BUCKET, VENDOR], ""));
    let (shard_url, _) = server(vec![http_ok(&shard)]).await;
    // The shard server also serves the index first, at the same port.
    let idx = index("o", "shards/o.json", &shard);
    let hits = Arc::new(AtomicUsize::new(0));
    let port = run_flaky_server(
        Arc::new(std::sync::Mutex::new(vec![http_ok(&idx), http_ok(&shard)])),
        hits.clone(),
    )
    .await;
    drop(shard_url);
    let source = format!("http://127.0.0.1:{port}/mirror/manifest.json");
    let m = load_with(&test_client(), &[source], None, CACHE_TTL)
        .await
        .unwrap();
    assert!(m.entry_for(VENDOR, Some("o")).await.is_some());
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

/// The registry API hands out some download URLs with a literal space; the
/// manifest lists them with `%20`. Both forms must find the entry, keep the
/// primary exactly as requested, and never list the vendor twice.
#[tokio::test]
async fn space_in_registry_url_matches_the_escaped_manifest_entry() {
    let _guard = network_test_guard().await;
    let _m = ScopedEnv::unset(MIRRORS_ENV);
    let _f = ScopedEnv::unset(MIRROR_FIRST_ENV);
    let escaped = "https://dl.registry.platformio.org/download/dmytro%20korniienko/library/EmbUI/2.7.0/EmbUI-2.7.0.tar.gz";
    let bucket = "https://fbuild.sfo3.digitaloceanspaces.com/dmytro%20korniienko/library/EmbUI/2.7.0/EmbUI-2.7.0.tar.gz";
    let spaced = escaped.replace("%20", " ");
    let m = parse(&doc(&component(
        &[bucket, escaped],
        &format!(r#","sha256":"{HELLO_SHA}""#),
    )));
    assert_eq!(super::mirrors::canonical_url(&spaced), escaped);
    assert_eq!(
        super::mirrors::registry_owner(escaped).as_deref(),
        Some("dmytro%20korniienko")
    );
    let entry = m.lookup(&super::mirrors::canonical_url(&spaced));
    let c = resolve_with(&spaced, entry);
    assert_eq!(c.urls, vec![spaced.clone(), bucket.to_string()]);
    assert_eq!(c.order, vec![1, 0]);
    assert_eq!(c.mirror_sha256.as_deref(), Some(HELLO_SHA));
}

/// A manifest entry that somehow lacks the requested URL still leaves the
/// primary in the attempt order.
#[tokio::test]
async fn primary_is_always_attempted() {
    let _guard = network_test_guard().await;
    let _m = ScopedEnv::unset(MIRRORS_ENV);
    let _f = ScopedEnv::unset(MIRROR_FIRST_ENV);
    let m = parse(&doc(&component(&[BUCKET, OTHER], "")));
    let c = resolve_with(VENDOR, m.lookup(BUCKET));
    assert_eq!(c.urls[0], VENDOR);
    assert!(c.order.contains(&0), "{c:?}");
}

// ------------------------------------------------- no PlatformIO extras

/// fbuild may talk to PlatformIO only through the primary download: no
/// PlatformIO mirror host and no `bypass=` URL is ever a candidate, with or
/// without a manifest (FastLED/fbuild#1815).
#[tokio::test]
async fn candidates_never_include_platformio_mirrors() {
    let _guard = network_test_guard().await;
    let _m = ScopedEnv::unset(MIRRORS_ENV);
    let _f = ScopedEnv::unset(MIRROR_FIRST_ENV);
    let m = parse(&doc(&component(&[BUCKET, VENDOR], "")));
    for entry in [None, m.lookup(VENDOR)] {
        let c = resolve_with(VENDOR, entry);
        assert_eq!(c.urls[0], VENDOR);
        for url in &c.urls[1..] {
            let host = url.split('/').nth(2).unwrap();
            assert!(!host.ends_with("platformio.org"), "{url}");
            assert!(!url.contains("bypass="), "{url}");
            assert!(!url.contains("pioreg"), "{url}");
        }
        assert_eq!(
            c.urls
                .iter()
                .filter(|u| u.contains("platformio.org"))
                .count(),
            1,
            "the primary is the only PlatformIO URL"
        );
    }
}

// ------------------------------------------------------- published documents

fn site_mirror() -> fbuild_core::path::NormalizedPath {
    fbuild_core::path::NormalizedPath::new(env!("CARGO_MANIFEST_DIR")).join("../../site/mirror")
}

/// The committed index and shards (what Pages serves) pass this reader's
/// validation: every shard hashes to its descriptor, and every entry puts
/// the bucket first with a sha256.
#[tokio::test]
async fn committed_manifest_is_accepted_and_prefers_the_bucket() {
    let _guard = network_test_guard().await;
    let _m = ScopedEnv::unset(MIRRORS_ENV);
    let _f = ScopedEnv::unset(MIRROR_FIRST_ENV);
    let bytes = std::fs::read(site_mirror().join("manifest.json").as_path()).unwrap();
    let m = MirrorManifest::parse(&bytes).expect("site/mirror/manifest.json must validate");
    let doc: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let tools = doc["tools"].as_object().expect("an Index");
    assert!(tools.contains_key("platformio"));
    let mut checked = 0;
    for owner in tools.keys() {
        let shard_ref = m.shard_ref(owner).unwrap();
        let shard_bytes = std::fs::read(site_mirror().join(&shard_ref.url).as_path()).unwrap();
        assert_eq!(sha_hex(&shard_bytes), shard_ref.sha256, "{owner}");
        assert_eq!(shard_bytes.len() as u64, shard_ref.size_bytes, "{owner}");
        let shard_doc: serde_json::Value = serde_json::from_slice(&shard_bytes).unwrap();
        let shard = MirrorManifest::parse(&shard_bytes).unwrap();
        for component in shard_doc["components"].as_array().unwrap() {
            let urls: Vec<&str> = component["asset"]["urls"]
                .as_array()
                .unwrap()
                .iter()
                .map(|u| u.as_str().unwrap())
                .collect();
            assert!(urls[0].starts_with("https://fbuild.sfo3.digitaloceanspaces.com/"));
            assert!(urls[1].starts_with("https://dl.registry.platformio.org/download/"));
            let c = resolve_with(urls[1], shard.lookup(urls[1]));
            assert_eq!(c.urls[c.order[0]], urls[0]);
            assert!(c.mirror_sha256.is_some());
            checked += 1;
        }
    }
    assert!(checked > 1000, "only {checked} entries");
}

/// Live: the published index is reachable and its platformio shard verifies.
#[tokio::test]
#[ignore = "network: fetches https://fastled.github.io/fbuild/mirror/manifest.json"]
async fn published_manifest_resolves_live() {
    let vendor = "https://dl.registry.platformio.org/download/platformio/tool/framework-arduino-avr/5.4.0/framework-arduino-avr-5.4.0.tar.gz";
    for source in super::manifest::manifest_urls() {
        let m = load_with(
            &test_client(),
            std::slice::from_ref(&source),
            None,
            CACHE_TTL,
        )
        .await
        .unwrap_or_else(|| panic!("{source} did not serve a valid manifest"));
        let entry = m
            .entry_for(vendor, Some("platformio"))
            .await
            .unwrap_or_else(|| panic!("{source}: framework-arduino-avr 5.4.0 not in its shard"));
        assert!(entry.urls[0].starts_with("https://fbuild.sfo3.digitaloceanspaces.com/"));
        assert!(entry.sha256.is_some());
    }
}
