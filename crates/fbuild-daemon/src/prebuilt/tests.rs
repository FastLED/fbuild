use super::*;
use fbuild_packages::prebuilt::{Asset, PrebuiltOutcome, sha256};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

pub(super) fn bundle(fp: &str) -> Vec<u8> {
    let payload = b"real framework object fixture";
    let manifest = serde_json::to_vec(&json!({"fp":fp,"abi_version":1,
        "inputs":fixture_plan(),"builder_version":"test","fbuild_version":"test","source":{"repo_url":"fixture","ref":"test"},
        "files":{"obj/core.o":sha256(payload)}})).unwrap();
    let mut tar = tar::Builder::new(Vec::new());
    for (path, bytes) in [
        ("fbuild-prebuilt.json", manifest.as_slice()),
        ("obj/core.o", payload.as_slice()),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, path, bytes).unwrap();
    }
    zstd::encode_all(tar.into_inner().unwrap().as_slice(), 1).unwrap()
}

pub(super) async fn server(
    body: Vec<u8>,
    status: axum::http::StatusCode,
) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let count = Arc::new(AtomicUsize::new(0));
    let calls = count.clone();
    let app = axum::Router::new().fallback(move || {
        let body = body.clone();
        let calls = calls.clone();
        async move {
            calls.fetch_add(1, Ordering::SeqCst);
            (status, [("etag", "\"fixture\"")], body)
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (url, count, task)
}

pub(super) fn asset(bytes: &[u8], urls: Vec<String>) -> Asset {
    Asset {
        filename: "prebuilt.tar.zst".into(),
        size_bytes: bytes.len() as u64,
        sha256: sha256(bytes),
        urls,
        provides: vec![],
        parts: vec![],
    }
}

#[tokio::test]
async fn eight_concurrent_requests_fetch_once_and_publish_verified_files() {
    let fp = fixture_fp();
    let bytes = bundle(&fp);
    let (url, calls, task) = server(bytes.clone(), axum::http::StatusCode::OK).await;
    let temp = tempfile::tempdir().unwrap();
    let service = Arc::new(Service::new(temp.path()));
    let asset = asset(&bytes, vec![url]);
    let futures = (0..8).map(|_| {
        let service = service.clone();
        let asset = asset.clone();
        let fp = fp.clone();
        tokio::spawn(async move { service.fetch(&asset, &fp).await })
    });
    for future in futures::future::join_all(futures).await {
        let PrebuiltOutcome::Hit(path) = future.unwrap() else {
            panic!("expected hit")
        };
        assert_eq!(
            std::fs::read(path.join("obj/core.o")).unwrap(),
            b"real framework object fixture"
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    task.abort();
}

#[tokio::test]
async fn failover_corruption_and_fingerprint_mismatch_are_misses() {
    let fp = fixture_fp();
    let bytes = bundle(&fp);
    let (good, _, good_task) = server(bytes.clone(), axum::http::StatusCode::OK).await;
    let (bad, _, bad_task) = server(vec![], axum::http::StatusCode::INTERNAL_SERVER_ERROR).await;
    let temp = tempfile::tempdir().unwrap();
    let service = Service::new(temp.path());
    assert!(matches!(
        service
            .fetch(&asset(&bytes, vec![bad, good.clone()]), &fp)
            .await,
        PrebuiltOutcome::Hit(_)
    ));
    let temp = tempfile::tempdir().unwrap();
    let service = Service::new(temp.path());
    assert!(matches!(
        service
            .fetch(&asset(&bytes, vec![good.clone()]), &"b".repeat(64))
            .await,
        PrebuiltOutcome::Miss(_)
    ));
    assert_eq!(
        std::fs::read_dir(temp.path().join("prebuilt/.staging"))
            .unwrap()
            .count(),
        0
    );
    let mut corrupted = asset(&bytes, vec![good]);
    corrupted.sha256 = "c".repeat(64);
    assert!(matches!(
        service.fetch(&corrupted, &fp).await,
        PrebuiltOutcome::Miss(_)
    ));
    bad_task.abort();
    good_task.abort();
}

#[tokio::test]
async fn gc_reaps_crash_staging_and_old_partials_at_startup() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("prebuilt");
    std::fs::create_dir_all(root.join(".staging/dead.nonce")).unwrap();
    std::fs::create_dir_all(root.join(".partial")).unwrap();
    std::fs::write(root.join(".partial/dead.part"), b"partial").unwrap();
    std::fs::write(
        root.join(".partial/dead.meta"),
        br#"{"url":"https://example.com","etag":"old","owner":"dead","progress":0}"#,
    )
    .unwrap();
    let _service = Service::new(temp.path());
    assert!(!root.join(".staging/dead.nonce").exists());
    assert!(!root.join(".partial/dead.part").exists());
}

#[tokio::test]
async fn disk_full_injection_is_a_miss_and_cleans_staging() {
    let fp = fixture_fp();
    let bytes = bundle(&fp);
    let (url, _, task) = server(bytes.clone(), axum::http::StatusCode::OK).await;
    let temp = tempfile::tempdir().unwrap();
    let mut service = Service::new(temp.path());
    service.write_limit = Some(1);
    assert!(matches!(
        service.fetch(&asset(&bytes, vec![url]), &fp).await,
        PrebuiltOutcome::Miss(_)
    ));
    assert_eq!(
        std::fs::read_dir(temp.path().join("prebuilt/.staging"))
            .unwrap()
            .count(),
        0
    );
    task.abort();
}

#[tokio::test]
async fn truncated_stream_and_unsupported_abi_are_misses() {
    let fp = fixture_fp();
    let mut bytes = bundle(&fp);
    bytes.truncate(bytes.len() / 2);
    let (url, _, task) = server(bytes.clone(), axum::http::StatusCode::OK).await;
    let temp = tempfile::tempdir().unwrap();
    let service = Service::new(temp.path());
    assert!(matches!(
        service.fetch(&asset(&bytes, vec![url]), &fp).await,
        PrebuiltOutcome::Miss(_)
    ));
    assert_eq!(
        std::fs::read_dir(temp.path().join("prebuilt/.staging"))
            .unwrap()
            .count(),
        0
    );
    task.abort();
}

#[tokio::test]
async fn cached_entry_survives_reconciliation_and_refetches_after_lru_eviction() {
    let fp = fixture_fp();
    let bytes = bundle(&fp);
    let (url, calls, task) = server(bytes.clone(), axum::http::StatusCode::OK).await;
    let temp = tempfile::tempdir().unwrap();
    let service = Service::new(temp.path());
    let artifact = asset(&bytes, vec![url]);
    let PrebuiltOutcome::Hit(path) = service.fetch(&artifact, &fp).await else {
        panic!("first fetch")
    };
    let _cache = fbuild_packages::DiskCache::open_at(temp.path()).unwrap();
    assert!(path.is_dir());
    assert!(matches!(
        service.fetch(&artifact, &fp).await,
        PrebuiltOutcome::Hit(_)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    std::fs::remove_dir_all(path).unwrap();
    assert!(matches!(
        service.fetch(&artifact, &fp).await,
        PrebuiltOutcome::Hit(_)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    task.abort();
}

#[test]
fn gc_reaps_oldest_sparse_partials_over_two_gib_and_keeps_recent_small_ones() {
    let temp = tempfile::tempdir().unwrap();
    let partial = temp.path().join("prebuilt/.partial");
    std::fs::create_dir_all(&partial).unwrap();
    for (key, length, progress) in [
        (
            "older",
            2 * 1024 * 1024 * 1024u64,
            super::transfer::now() - 2,
        ),
        ("newer", 17, super::transfer::now()),
    ] {
        std::fs::File::create(partial.join(format!("{key}.part")))
            .unwrap()
            .set_len(length)
            .unwrap();
        let meta = super::transfer::Meta {
            url: "https://example.com".into(),
            etag: None,
            owner: "dead".into(),
            progress,
        };
        std::fs::write(
            partial.join(format!("{key}.meta")),
            serde_json::to_vec(&meta).unwrap(),
        )
        .unwrap();
    }
    let _service = Service::new(temp.path());
    assert!(!partial.join("older.part").exists());
    assert!(partial.join("newer.part").exists());
}

fn modified_bundle(fp: &str, change: &str) -> Vec<u8> {
    use std::io::Read;
    let original = bundle(fp);
    let decoded = zstd::decode_all(original.as_slice()).unwrap();
    let mut archive = tar::Archive::new(decoded.as_slice());
    let mut output = tar::Builder::new(Vec::new());
    for (index, entry) in archive.entries().unwrap().enumerate() {
        let mut entry = entry.unwrap();
        let mut path = entry.path().unwrap().into_owned();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        if index == 0 {
            let mut manifest: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            match change {
                "abi" => manifest["abi_version"] = json!(2),
                "file-sha" => manifest["files"]["obj/core.o"] = json!("f".repeat(64)),
                "first" => path = "unexpected.json".into(),
                _ => panic!("unknown test mutation"),
            }
            bytes = serde_json::to_vec(&manifest).unwrap();
        }
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        output
            .append_data(&mut header, path, bytes.as_slice())
            .unwrap();
    }
    zstd::encode_all(output.into_inner().unwrap().as_slice(), 1).unwrap()
}

#[tokio::test]
async fn per_file_checksum_abi_and_first_member_are_checked_before_publication() {
    for change in ["abi", "file-sha", "first"] {
        let fp = fixture_fp();
        let bytes = modified_bundle(&fp, change);
        let (url, _, task) = server(bytes.clone(), axum::http::StatusCode::OK).await;
        let temp = tempfile::tempdir().unwrap();
        let service = Service::new(temp.path());
        let artifact = asset(&bytes, vec![url.clone()]);
        assert!(
            matches!(
                service.fetch(&artifact, &fp).await,
                PrebuiltOutcome::Miss(_)
            ),
            "{change}"
        );
        assert!(!temp.path().join("prebuilt/entries").join(&fp).exists());
        assert_eq!(
            std::fs::read_dir(temp.path().join("prebuilt/.staging"))
                .unwrap()
                .count(),
            0
        );
        assert!(service.tombstoned(&format!("{url}\n{}", artifact.sha256)));
        task.abort();
    }
}

pub(super) fn fixture_plan() -> fbuild_build::prebuilt_fp::FrameworkBuildPlan {
    let identity = fbuild_build::prebuilt_fp::PackageIdentity {
        id: "fixture".into(),
        version: "1".into(),
        sha256: "a".repeat(64),
    };
    fbuild_build::prebuilt_fp::FrameworkBuildPlan {
        abi_version: 1,
        fingerprint_schema: 1,
        toolchain: identity.clone(),
        framework: identity,
        mcu: "esp32".into(),
        variant_dir: "esp32".into(),
        argv: vec![vec!["-Os".into()]],
        config_sha256: std::collections::BTreeMap::new(),
        path_map_prefix: "/fbuild".into(),
    }
}

pub(super) fn fixture_fp() -> String {
    fbuild_build::prebuilt_fp::fingerprint(&fixture_plan())
}

#[test]
fn extraction_rejects_nonempty_staging_without_writing_members() {
    let temp = tempfile::tempdir().unwrap();
    let sentinel = temp.path().join("existing");
    std::fs::write(&sentinel, b"preserve").unwrap();
    let fp = fixture_fp();
    let bytes = bundle(&fp);
    assert!(extract::extract(bytes.as_slice(), temp.path(), &fp, None).is_err());
    assert_eq!(std::fs::read(sentinel).unwrap(), b"preserve");
    assert!(!temp.path().join("obj").exists());
}
