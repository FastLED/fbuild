use super::*;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

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

fn catalog(fp: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({"kind":"Catalog","schema_version":1,
        "tool":"prebuilt-arduino-esp32", "channels":{"3.1.1":"3.1.1"},
        "releases":[{"tool":"prebuilt-arduino-esp32", "version":"3.1.1",
            "platforms":[{"platform":{"os":"none","arch":"esp32","abi":"gcc@1"},
                "variant":{"optimization":"Os", "flavor":format!("fp-{}", &fp[..16])},
                "asset":{"filename":"prebuilt.tar.zst","size_bytes":5,"sha256":"a".repeat(64),
                    "urls":["https://example.com/prebuilt.tar.zst"],"provides":[format!("fbuild-fp:{fp}")]}}]}]})).unwrap()
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

#[tokio::test]
async fn no_index_is_a_miss() {
    let temp = tempfile::tempdir().unwrap();
    let reader = Reader::new(temp.path(), vec![], reqwest::Client::new());
    assert!(matches!(
        reader.resolve(&query(&"b".repeat(64))).await,
        Resolution::Miss(_)
    ));
}

#[tokio::test]
async fn exact_match_offline_and_descriptor_integrity() {
    let fp = "b".repeat(64);
    let shard = catalog(&fp);
    let (shard_url, calls, shard_task) = server(shard.clone()).await;
    let index = serde_json::to_vec(&json!({"kind":"Index","schema_version":1,
        "tools":{"prebuilt-arduino-esp32":{"descriptor":{"url":shard_url,
            "sha256":digest(&shard),"size_bytes":shard.len()}}}}))
    .unwrap();
    let (index_url, _, index_task) = server(index).await;
    let temp = tempfile::tempdir().unwrap();
    let reader = Reader::new(temp.path(), vec![index_url], reqwest::Client::new());
    assert!(matches!(
        reader.resolve(&query(&fp)).await,
        Resolution::Hit(_)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    index_task.abort();
    shard_task.abort();
    assert!(matches!(
        reader
            .resolve_with_ttl(&query(&fp), std::time::Duration::ZERO)
            .await,
        Resolution::Hit(_)
    ));
    let mut wrong = fp.clone();
    wrong.replace_range(16..17, "a");
    assert!(matches!(
        reader.resolve(&query(&wrong)).await,
        Resolution::Miss(_)
    ));
    std::fs::write(reader.shard_cache("prebuilt-arduino-esp32"), b"corrupt").unwrap();
    assert!(matches!(
        reader.resolve(&query(&fp)).await,
        Resolution::Miss(_)
    ));
}

#[tokio::test]
async fn shard_sha_and_exact_size_are_checked_on_network_and_last_good_reads() {
    let fp = "b".repeat(64);
    let shard = catalog(&fp);
    for (sha, size) in [
        ("a".repeat(64), shard.len()),
        (digest(&shard), shard.len() + 1),
    ] {
        let (shard_url, _, shard_task) = server(shard.clone()).await;
        let index=serde_json::to_vec(&json!({"kind":"Index","schema_version":1,"tools":{
            "prebuilt-arduino-esp32":{"descriptor":{"url":shard_url,"sha256":sha,"size_bytes":size}}}})).unwrap();
        let (index_url, _, index_task) = server(index).await;
        let temp = tempfile::tempdir().unwrap();
        let reader = Reader::new(temp.path(), vec![index_url], reqwest::Client::new());
        assert!(matches!(
            reader.resolve(&query(&fp)).await,
            Resolution::Miss(_)
        ));
        std::fs::create_dir_all(
            reader
                .shard_cache("prebuilt-arduino-esp32")
                .parent()
                .unwrap(),
        )
        .unwrap();
        std::fs::write(reader.shard_cache("prebuilt-arduino-esp32"), &shard).unwrap();
        index_task.abort();
        shard_task.abort();
        assert!(
            matches!(
                reader
                    .resolve_with_ttl(&query(&fp), std::time::Duration::ZERO)
                    .await,
                Resolution::Miss(_)
            ),
            "a stale cache read must still be verified against the current descriptor"
        );
    }
}

#[tokio::test]
async fn a_catalog_that_does_not_claim_the_exact_fingerprint_is_a_miss() {
    let fp = "b".repeat(64);
    let mut value: serde_json::Value = serde_json::from_slice(&catalog(&fp)).unwrap();
    let platforms = &mut value["releases"][0]["platforms"];
    // A fingerprint PREFIX flavor plus another fingerprint's provides entry is
    // exactly the near-miss a channel-style lookup used to allow.
    platforms[0]["variant"]["flavor"] = json!(format!("fp-{}", &fp[..16]));
    platforms[0]["asset"]["provides"] = json!([format!("fbuild-fp:{}", "c".repeat(64))]);
    let shard = serde_json::to_vec(&value).unwrap();
    let (shard_url, _, shard_task) = server(shard.clone()).await;
    let index = serde_json::to_vec(&json!({"kind":"Index","schema_version":1,
        "tools":{"prebuilt-arduino-esp32":{"descriptor":{"url":shard_url,
            "sha256":digest(&shard),"size_bytes":shard.len()}}}}))
    .unwrap();
    let (index_url, _, index_task) = server(index).await;
    let temp = tempfile::tempdir().unwrap();
    let reader = Reader::new(temp.path(), vec![index_url], reqwest::Client::new());
    assert!(matches!(
        reader.resolve(&query(&fp)).await,
        Resolution::Miss(_)
    ));
    index_task.abort();
    shard_task.abort();
}

#[test]
fn explicit_operator_overrides_and_mirror_order() {
    let asset = Asset {
        filename: "prebuilt.tar.zst".into(),
        size_bytes: 1,
        sha256: "a".repeat(64),
        urls: vec![
            "https://published/one".into(),
            "https://published/two".into(),
        ],
        provides: vec![],
        parts: vec![],
    };
    let query = query(&"b".repeat(64));
    assert_eq!(operator_urls(&asset, &query, None, true), asset.urls);
    assert!(operator_urls(&asset, &query, Some(""), true).is_empty());
    let urls = operator_urls(&asset, &query, Some("https://operator/{path}"), true);
    assert_eq!(
        urls[0],
        format!(
            "https://operator/prebuilt/arduino-esp32/3.1.1/esp32/fp-{}/prebuilt.tar.zst",
            "b".repeat(16)
        )
    );
    let urls = operator_urls(&asset, &query, Some("https://operator"), false);
    assert_eq!(
        &urls[..2],
        &["https://published/one", "https://operator/prebuilt.tar.zst"]
    );
}

#[test]
fn identity_helpers_reject_short_upper_and_empty_values() {
    assert!(valid_sha(&"a".repeat(64)));
    assert!(!valid_sha(&"a".repeat(63)));
    assert!(!valid_sha(&"A".repeat(64)));
    assert_eq!(sha256(b"").len(), 64);
    assert_eq!(
        sha256(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}
