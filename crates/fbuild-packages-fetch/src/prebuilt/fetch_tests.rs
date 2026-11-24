//! Local-HTTP fixtures for the prebuilt fetch ladder (#1882).
use super::*;
use axum::{
    Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicUsize, Ordering};

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn body(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

#[derive(Clone, Default)]
struct Plan {
    body: Arc<String>,
    etag: Arc<String>,
    ignore_range: bool,
    corrupt: bool,
    delay: u64,
    count: Arc<AtomicUsize>,
    seen: Arc<Mutex<Vec<(Option<String>, Option<String>)>>>,
}

impl Plan {
    fn new(body: &[u8]) -> Self {
        Self {
            body: Arc::new(String::new()),
            etag: Arc::new("\"v1\"".into()),
            count: Arc::new(AtomicUsize::new(0)),
            seen: Arc::new(Mutex::new(Vec::new())),
            ..Self::default()
        }
        .with_body(body)
    }

    fn with_body(mut self, body: &[u8]) -> Self {
        // Latin-1 round trip keeps arbitrary bytes intact in a String.
        self.body = Arc::new(body.iter().map(|b| *b as char).collect());
        self
    }

    fn bytes(&self) -> Vec<u8> {
        self.body.chars().map(|c| c as u8).collect()
    }
}

async fn serve(State(plan): State<Plan>, headers: HeaderMap) -> Response {
    plan.count.fetch_add(1, Ordering::SeqCst);
    let header = |name: axum::http::HeaderName| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };
    let range = header(axum::http::header::RANGE);
    let if_range = header(axum::http::header::IF_RANGE);
    plan.seen
        .lock()
        .unwrap()
        .push((range.clone(), if_range.clone()));
    if plan.delay > 0 {
        tokio::time::sleep(Duration::from_millis(plan.delay)).await;
    }
    let mut bytes = plan.bytes();
    if plan.corrupt {
        bytes[0] ^= 0xff;
    }
    let etag = axum::http::HeaderValue::from_str(&plan.etag).unwrap();
    if let (Some(range), false) = (range, plan.ignore_range) {
        let start: usize = range
            .trim_start_matches("bytes=")
            .split('-')
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        if start >= bytes.len() {
            return (StatusCode::RANGE_NOT_SATISFIABLE, "range").into_response();
        }
        let mut response = (StatusCode::PARTIAL_CONTENT, bytes[start..].to_vec()).into_response();
        response.headers_mut().insert(
            axum::http::header::CONTENT_RANGE,
            format!("bytes {start}-{}/{}", bytes.len() - 1, bytes.len())
                .parse()
                .unwrap(),
        );
        response
            .headers_mut()
            .insert(axum::http::header::ETAG, etag);
        return response;
    }
    let mut response = (StatusCode::OK, bytes).into_response();
    response
        .headers_mut()
        .insert(axum::http::header::ETAG, etag);
    response
}

async fn server(plan: Plan) -> (String, Plan, tokio::task::JoinHandle<()>) {
    let app = Router::new().fallback(serve).with_state(plan.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (address, plan, handle)
}

fn asset(url: &str, bytes: &[u8]) -> Asset {
    Asset {
        filename: "prebuilt.tar.zst".into(),
        size_bytes: bytes.len() as u64,
        sha256: digest(bytes),
        urls: vec![url.to_owned()],
        provides: vec![format!("fbuild-fp:{}", "b".repeat(64))],
        parts: Vec::new(),
    }
}

fn fetcher(dest: &std::path::Path) -> Fetcher {
    Fetcher::new(dest, crate::http::client().clone())
}

#[tokio::test]
async fn downloads_verifies_and_then_serves_from_disk_without_network() {
    let bytes = body(4096);
    let (url, plan, task) = server(Plan::new(&bytes)).await;
    let temp = tempfile::tempdir().unwrap();
    let fetcher = fetcher(temp.path());
    let artifact = fetcher.fetch(&asset(&url, &bytes)).await.unwrap();
    assert_eq!(artifact.size_bytes, 4096);
    assert_eq!(std::fs::read(artifact.path.as_path()).unwrap(), bytes);
    assert_eq!(plan.count.load(Ordering::SeqCst), 1);
    task.abort();
    let again = fetcher.fetch(&asset(&url, &bytes)).await.unwrap();
    assert_eq!(again.path, artifact.path);
    assert_eq!(
        plan.count.load(Ordering::SeqCst),
        1,
        "verified file must not re-fetch"
    );
}

#[tokio::test]
async fn resumes_a_partial_body_with_range_and_if_range() {
    let bytes = body(8192);
    let (url, plan, task) = server(Plan::new(&bytes)).await;
    let temp = tempfile::tempdir().unwrap();
    let fetcher = fetcher(temp.path());
    let digest = digest(&bytes);
    let partial = temp.path().join(format!(".{digest}.partial"));
    std::fs::write(&partial, &bytes[..3000]).unwrap();
    std::fs::write(
        temp.path().join(format!(".{digest}.partial.json")),
        serde_json::to_vec(&Partial {
            sha256: digest.clone(),
            etag: Some("\"v1\"".into()),
            last_modified: None,
        })
        .unwrap(),
    )
    .unwrap();
    let artifact = fetcher.fetch(&asset(&url, &bytes)).await.unwrap();
    assert_eq!(std::fs::read(artifact.path.as_path()).unwrap(), bytes);
    let seen = plan.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0.as_deref(), Some("bytes=3000-"));
    assert_eq!(seen[0].1.as_deref(), Some("\"v1\""));
    task.abort();
}

#[tokio::test]
async fn restarts_from_zero_when_the_server_refuses_the_range() {
    let bytes = body(5000);
    let (url, plan, task) = server(Plan {
        ignore_range: true,
        ..Plan::new(&bytes)
    })
    .await;
    let temp = tempfile::tempdir().unwrap();
    let digest = digest(&bytes);
    let partial = temp.path().join(format!(".{digest}.partial"));
    std::fs::write(&partial, &bytes[..2000]).unwrap();
    std::fs::write(
        temp.path().join(format!(".{digest}.partial.json")),
        serde_json::to_vec(&Partial {
            sha256: digest.clone(),
            etag: Some("\"stale\"".into()),
            last_modified: None,
        })
        .unwrap(),
    )
    .unwrap();
    let artifact = fetcher(temp.path())
        .fetch(&asset(&url, &bytes))
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(artifact.path.as_path()).unwrap(),
        bytes,
        "a refused range must rewrite the body, not append to it"
    );
    assert_eq!(plan.count.load(Ordering::SeqCst), 1);
    task.abort();
}

#[tokio::test]
async fn a_416_discards_the_partial_and_the_fresh_attempt_succeeds() {
    let bytes = body(2048);
    let (url, plan, task) = server(Plan::new(&bytes)).await;
    let temp = tempfile::tempdir().unwrap();
    let digest = digest(&bytes);
    std::fs::write(
        temp.path().join(format!(".{digest}.partial")),
        vec![0u8; 9000],
    )
    .unwrap();
    std::fs::write(
        temp.path().join(format!(".{digest}.partial.json")),
        serde_json::to_vec(&Partial {
            sha256: digest.clone(),
            etag: None,
            last_modified: None,
        })
        .unwrap(),
    )
    .unwrap();
    let artifact = fetcher(temp.path())
        .fetch(&asset(&url, &bytes))
        .await
        .unwrap();
    assert_eq!(std::fs::read(artifact.path.as_path()).unwrap(), bytes);
    assert_eq!(plan.count.load(Ordering::SeqCst), 1);
    task.abort();
}

#[tokio::test]
async fn a_corrupt_source_falls_through_to_the_next_url() {
    let bytes = body(3000);
    let (bad_url, bad, bad_task) = server(Plan {
        corrupt: true,
        ..Plan::new(&bytes)
    })
    .await;
    let (good_url, good, good_task) = server(Plan::new(&bytes)).await;
    let temp = tempfile::tempdir().unwrap();
    let mut spec = asset(&bad_url, &bytes);
    spec.urls.push(good_url);
    let artifact = fetcher(temp.path()).fetch(&spec).await.unwrap();
    assert_eq!(std::fs::read(artifact.path.as_path()).unwrap(), bytes);
    assert!(bad.count.load(Ordering::SeqCst) >= 1);
    assert_eq!(good.count.load(Ordering::SeqCst), 1);
    bad_task.abort();
    good_task.abort();
}

#[tokio::test]
async fn every_corrupt_source_is_exhausted_and_leaves_no_partial() {
    let bytes = body(1500);
    let (url, _plan, task) = server(Plan {
        corrupt: true,
        ..Plan::new(&bytes)
    })
    .await;
    let temp = tempfile::tempdir().unwrap();
    let digest = digest(&bytes);
    let error = fetcher(temp.path())
        .fetch(&asset(&url, &bytes))
        .await
        .unwrap_err();
    assert!(matches!(error, FetchError::Exhausted { .. }), "{error}");
    assert!(!temp.path().join(format!(".{digest}.partial")).exists());
    assert!(!temp.path().join(format!(".{digest}.partial.json")).exists());
    task.abort();
}

#[tokio::test]
async fn an_over_long_body_is_refused_before_writing_past_the_declared_size() {
    let bytes = body(1000);
    let (url, _plan, task) = server(Plan::new(&body(2000))).await;
    let temp = tempfile::tempdir().unwrap();
    let spec = asset(&url, &bytes); // declares 1000, server sends 2000
    let error = fetcher(temp.path()).fetch(&spec).await.unwrap_err();
    assert!(matches!(error, FetchError::Exhausted { .. }), "{error}");
    let digest = digest(&bytes);
    assert!(!temp.path().join(format!(".{digest}.partial")).exists());
    task.abort();
}

#[tokio::test]
async fn a_short_body_is_kept_as_a_resumable_partial() {
    let declared = body(4000);
    let (url, _plan, task) = server(Plan::new(&body(1200))).await;
    let temp = tempfile::tempdir().unwrap();
    let error = fetcher(temp.path())
        .fetch(&asset(&url, &declared))
        .await
        .unwrap_err();
    assert!(matches!(error, FetchError::Exhausted { .. }), "{error}");
    let partial = temp.path().join(format!(".{}.partial", digest(&declared)));
    assert_eq!(std::fs::metadata(partial).unwrap().len(), 1200);
    task.abort();
}

#[tokio::test]
async fn concurrent_fetches_of_one_digest_collapse_to_a_single_download() {
    let bytes = body(64 * 1024);
    let (url, plan, task) = server(Plan {
        delay: 300,
        ..Plan::new(&bytes)
    })
    .await;
    let temp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(fetcher(temp.path()));
    let spec = Arc::new(asset(&url, &bytes));
    let mut handles = Vec::new();
    for _ in 0..4 {
        let fetcher = fetcher.clone();
        let spec = spec.clone();
        handles.push(tokio::spawn(async move { fetcher.fetch(&spec).await }));
    }
    for handle in handles {
        let artifact = handle.await.unwrap().unwrap();
        assert_eq!(artifact.size_bytes, bytes.len() as u64);
    }
    assert_eq!(plan.count.load(Ordering::SeqCst), 1);
    task.abort();
}

#[tokio::test]
async fn invalid_assets_are_refused_without_a_request() {
    let bytes = body(10);
    let (url, plan, task) = server(Plan::new(&bytes)).await;
    let temp = tempfile::tempdir().unwrap();
    let fetcher = fetcher(temp.path());
    let mut spec = asset(&url, &bytes);
    spec.sha256 = "A".repeat(64);
    assert!(matches!(
        fetcher.fetch(&spec).await.unwrap_err(),
        FetchError::Invalid(_)
    ));
    let mut spec = asset(&url, &bytes);
    spec.size_bytes = 0;
    assert!(matches!(
        fetcher.fetch(&spec).await.unwrap_err(),
        FetchError::Invalid(_)
    ));
    let mut spec = asset(&url, &bytes);
    spec.parts = vec![serde_json::json!({"url": "https://example.com/2"})];
    assert!(matches!(
        fetcher.fetch(&spec).await.unwrap_err(),
        FetchError::Invalid(_)
    ));
    let mut spec = asset(&url, &bytes);
    spec.urls = vec!["file:///etc/passwd".into()];
    assert!(matches!(
        fetcher.fetch(&spec).await.unwrap_err(),
        FetchError::Exhausted { .. }
    ));
    assert_eq!(plan.count.load(Ordering::SeqCst), 0);
    task.abort();
}

#[tokio::test]
async fn an_asset_over_the_limit_is_refused_before_any_request() {
    let bytes = body(4096);
    let (url, plan, task) = server(Plan::new(&bytes)).await;
    let temp = tempfile::tempdir().unwrap();
    let fetcher = Fetcher::with_limit(temp.path(), crate::http::client().clone(), 1024);
    let error = fetcher.fetch(&asset(&url, &bytes)).await.unwrap_err();
    assert!(matches!(
        error,
        FetchError::TooLarge {
            declared: 4096,
            limit: 1024
        }
    ));
    assert_eq!(plan.count.load(Ordering::SeqCst), 0);
    task.abort();
}

#[tokio::test]
async fn a_traversing_filename_cannot_escape_the_destination() {
    let bytes = body(512);
    let (url, _plan, task) = server(Plan::new(&bytes)).await;
    let temp = tempfile::tempdir().unwrap();
    let mut spec = asset(&url, &bytes);
    spec.filename = "../../evil.tar.zst".into();
    let artifact = fetcher(temp.path()).fetch(&spec).await.unwrap();
    let root = NormalizedPath::new(temp.path());
    assert!(artifact.path.relative_to(&root).is_some());
    assert!(!artifact.path.as_path().to_string_lossy().contains(".."));
    task.abort();
}
