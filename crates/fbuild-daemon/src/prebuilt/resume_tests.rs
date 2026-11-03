//! Real HTTP Range/If-Range, stalls, live GC and process-death recovery.
use super::{
    Service,
    tests::{asset, bundle, server},
    transfer::{Meta, now},
};
use axum::{
    body::Body,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use fbuild_packages::prebuilt::{Asset, PrebuiltOutcome};
use futures::StreamExt;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

async fn range_server(
    bytes: Vec<u8>,
    changed: bool,
    stall_first: bool,
) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let count = Arc::new(AtomicUsize::new(0));
    let calls = count.clone();
    let app = axum::Router::new().fallback(move |headers: HeaderMap| {
        let bytes = bytes.clone();
        let count = count.clone();
        async move {
            let n = count.fetch_add(1, Ordering::SeqCst);
            if stall_first && n == 0 {
                let first = bytes[..bytes.len() / 2].to_vec();
                let stream = futures::stream::once(async move {
                    Ok::<_, std::io::Error>(bytes::Bytes::from(first))
                })
                .chain(futures::stream::pending());
                return Response::builder()
                    .header(header::ETAG, "\"old\"")
                    .body(Body::from_stream(stream))
                    .unwrap();
            }
            if let Some(range) = headers.get(header::RANGE) {
                assert_eq!(headers.get(header::IF_RANGE).unwrap(), "\"old\"");
                let start: usize = range
                    .to_str()
                    .unwrap()
                    .trim_start_matches("bytes=")
                    .trim_end_matches('-')
                    .parse()
                    .unwrap();
                if !changed {
                    return (
                        StatusCode::PARTIAL_CONTENT,
                        [
                            (header::ETAG, "\"old\"".to_owned()),
                            (
                                header::CONTENT_RANGE,
                                format!("bytes {start}-{}/{}", bytes.len() - 1, bytes.len()),
                            ),
                        ],
                        bytes[start..].to_vec(),
                    )
                        .into_response();
                }
            }
            (
                StatusCode::OK,
                [(header::ETAG, if changed { "\"new\"" } else { "\"old\"" })],
                bytes,
            )
                .into_response()
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (url, calls, task)
}

#[tokio::test]
async fn resume_redecodes_raw_prefix_and_changed_etag_restarts_from_zero() {
    for changed in [false, true] {
        let fp = super::tests::fixture_fp();
        let bytes = bundle(&fp);
        let (url, calls, task) = range_server(bytes.clone(), changed, false).await;
        let temp = tempfile::tempdir().unwrap();
        let artifact = asset(&bytes, vec![url.clone()]);
        let partial = temp.path().join("prebuilt/.partial");
        std::fs::create_dir_all(&partial).unwrap();
        std::fs::write(
            partial.join(format!("{}.part", artifact.sha256)),
            &bytes[..bytes.len() / 2],
        )
        .unwrap();
        let meta = Meta {
            url,
            etag: Some("\"old\"".into()),
            owner: "dead-daemon".into(),
            progress: now(),
        };
        std::fs::write(
            partial.join(format!("{}.meta", artifact.sha256)),
            serde_json::to_vec(&meta).unwrap(),
        )
        .unwrap();
        let service = Service::new(temp.path());
        assert!(matches!(
            service.fetch(&artifact, &fp).await,
            PrebuiltOutcome::Hit(_)
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        task.abort();
    }
}

#[tokio::test]
async fn not_found_and_stalled_first_mirrors_fail_over() {
    for stall in [false, true] {
        let fp = super::tests::fixture_fp();
        let bytes = bundle(&fp);
        let (bad, _, bad_task) = if stall {
            range_server(bytes.clone(), false, true).await
        } else {
            server(vec![], StatusCode::NOT_FOUND).await
        };
        let (good, _, good_task) = server(bytes.clone(), StatusCode::OK).await;
        let temp = tempfile::tempdir().unwrap();
        let mut service = Service::new(temp.path());
        service.stall_timeout = std::time::Duration::from_millis(30);
        assert!(matches!(
            service.fetch(&asset(&bytes, vec![bad, good]), &fp).await,
            PrebuiltOutcome::Hit(_)
        ));
        bad_task.abort();
        good_task.abort();
    }
}

async fn wait_for_partial(path: &std::path::Path) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(10));
        loop {
            if std::fs::metadata(path).is_ok_and(|m| m.len() > 0) {
                break;
            }
            interval.tick().await;
        }
    })
    .await
    .expect("download must persist its compressed prefix");
}

#[tokio::test]
async fn gc_does_not_touch_a_live_partial_or_staging_directory() {
    let fp = super::tests::fixture_fp();
    let bytes = bundle(&fp);
    let (url, _, task) = range_server(bytes.clone(), false, true).await;
    let temp = tempfile::tempdir().unwrap();
    let service = Arc::new(Service::new(temp.path()));
    let artifact = asset(&bytes, vec![url]);
    let sha = artifact.sha256.clone();
    let worker = service.clone();
    let download = tokio::spawn(async move { worker.fetch(&artifact, &fp).await });
    let part = temp.path().join(format!("prebuilt/.partial/{sha}.part"));
    wait_for_partial(&part).await;
    service.gc().unwrap();
    assert!(part.exists());
    assert_eq!(
        std::fs::read_dir(temp.path().join("prebuilt/.staging"))
            .unwrap()
            .count(),
        1
    );
    download.abort();
    let _ = download.await;
    task.abort();
}

#[test]
#[ignore = "subprocess helper for crash-recovery test #1882"]
fn crash_child() {
    let root = std::env::var("FBUILD_TEST_PREBUILT_ROOT").unwrap();
    let artifact: Asset =
        serde_json::from_str(&std::env::var("FBUILD_TEST_PREBUILT_ASSET").unwrap()).unwrap();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let service = Service::new(std::path::Path::new(&root));
        let _ = service.fetch(&artifact, &super::tests::fixture_fp()).await;
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn killed_download_resumes_in_a_new_daemon_with_the_correct_sha() {
    let fp = super::tests::fixture_fp();
    let bytes = bundle(&fp);
    let (url, calls, task) = range_server(bytes.clone(), false, true).await;
    let temp = tempfile::tempdir().unwrap();
    let artifact = asset(&bytes, vec![url]);
    // allow-direct-spawn: real crash-recovery child, spawned through the containment facade.
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "prebuilt::resume_tests::crash_child",
            "--ignored",
            "--nocapture",
        ])
        .env("FBUILD_TEST_PREBUILT_ROOT", temp.path())
        .env(
            "FBUILD_TEST_PREBUILT_ASSET",
            serde_json::to_string(&artifact).unwrap(),
        )
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut child = fbuild_core::platform::process::spawn_contained(
        &mut command,
        fbuild_core::platform::process::ContainedStdio {
            stdin: fbuild_core::platform::process::StdioSource::Null,
            stdout: fbuild_core::platform::process::StdioSource::Null,
            stderr: fbuild_core::platform::process::StdioSource::Null,
        },
    )
    .unwrap();
    let part = temp
        .path()
        .join(format!("prebuilt/.partial/{}.part", artifact.sha256));
    wait_for_partial(&part).await;
    child.kill().unwrap();
    child.wait().unwrap();
    let service = Service::new(temp.path());
    assert_eq!(
        std::fs::read_dir(temp.path().join("prebuilt/.staging"))
            .unwrap()
            .count(),
        0
    );
    let PrebuiltOutcome::Hit(entry) = service.fetch(&artifact, &fp).await else {
        panic!("resume must hit")
    };
    assert_eq!(
        std::fs::read(entry.join("obj/core.o")).unwrap(),
        b"real framework object fixture"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    task.abort();
}
