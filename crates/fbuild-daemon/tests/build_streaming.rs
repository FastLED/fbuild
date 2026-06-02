//! Streaming build endpoint regression tests.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::routing::post;
use axum::Router;
use fbuild_daemon::context::DaemonContext;
use fbuild_daemon::handlers::operations;

fn build_test_app(ctx: Arc<DaemonContext>) -> Router {
    Router::new()
        .route("/api/build", post(operations::build))
        .with_state(ctx)
}

fn make_test_context() -> Arc<DaemonContext> {
    let (tx, _rx) = tokio::sync::watch::channel(false);
    Arc::new(DaemonContext::new(
        0,
        tx,
        "build-streaming-test".to_string(),
    ))
}

async fn spawn_test_server(ctx: Arc<DaemonContext>) -> SocketAddr {
    let app = build_test_app(ctx);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("axum::serve should not fail in test");
    });
    addr
}

#[tokio::test]
async fn streaming_build_failure_emits_log_before_result() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("platformio.ini"),
        "[env:bad]\nplatform = ststm32\nboard = definitely_unknown_board\nframework = arduino\n",
    )
    .unwrap();

    let ctx = make_test_context();
    let addr = spawn_test_server(ctx).await;
    let body = serde_json::json!({
        "project_dir": tmp.path().display().to_string(),
        "environment": "bad",
        "stream": true,
        "verbose": false,
    });

    let resp = reqwest::Client::new()
        .post(format!("http://{}/api/build", addr))
        .json(&body)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .expect("POST /api/build should not drop the connection");

    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let text = resp.text().await.expect("read NDJSON body");
    let events: Vec<serde_json::Value> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("valid NDJSON event"))
        .collect();

    let log_index = events
        .iter()
        .position(|event| {
            event.get("type").and_then(|v| v.as_str()) == Some("log")
                && event
                    .get("message")
                    .and_then(|v| v.as_str())
                    .is_some_and(|msg| msg.contains("build error:"))
        })
        .expect("stream should include a build-error log event");
    let result_index = events
        .iter()
        .position(|event| event.get("type").and_then(|v| v.as_str()) == Some("result"))
        .expect("stream should include a final result event");

    assert!(
        log_index < result_index,
        "failure log must precede result event; events={events:?}"
    );
    assert_eq!(
        events[result_index]
            .get("success")
            .and_then(|v| v.as_bool()),
        Some(false)
    );
}
