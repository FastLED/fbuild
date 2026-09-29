//! Integration test for `GET /plotter` (FastLED/fbuild#1076 Phase 2).
//!
//! Mirrors the `test_emu_endpoint.rs` pattern: build a minimal `Router`
//! wired exactly like `main.rs`, spawn it on an ephemeral port, and assert
//! the route round-trips with the expected content — catching route-
//! registration regressions without needing the full production binary.

use axum::Router;
use axum::routing::get;
use fbuild_daemon::handlers::plotter;
use std::net::SocketAddr;
use std::time::Duration;

fn build_test_app() -> Router {
    Router::new().route("/plotter", get(plotter::plotter_page))
}

async fn spawn_test_server() -> SocketAddr {
    let app = build_test_app();
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

/// The `/plotter` route is registered and serves a self-contained HTML
/// page that attaches to the existing `/ws/serial-monitor` WebSocket.
#[tokio::test]
async fn plotter_route_serves_html_page() {
    let addr = spawn_test_server().await;

    let resp = fbuild_core::http::client_with_timeout(Duration::from_secs(10))
        .get(format!("http://{}/plotter", addr))
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .expect("GET /plotter should not drop the connection");

    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        content_type.starts_with("text/html"),
        "expected text/html, got {content_type}"
    );

    let body = resp.text().await.expect("body should be readable");
    assert!(
        body.contains("/ws/serial-monitor"),
        "must use the existing serial monitor websocket"
    );
    assert!(body.contains("<title>fbuild Serial Plotter</title>"));
}
