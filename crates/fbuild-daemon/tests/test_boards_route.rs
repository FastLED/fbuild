//! Integration test for `GET /boards` and `GET /api/ide/boards`
//! (FastLED/fbuild#1076 Phase 2).
//!
//! Mirrors `test_plotter_route.rs`: build a minimal `Router` wired exactly
//! like `main.rs`, spawn it on an ephemeral port, and assert both routes
//! round-trip with the expected content — catching route-registration
//! regressions without needing the full production binary.

use axum::Router;
use axum::routing::get;
use fbuild_daemon::handlers::boards;
use std::net::SocketAddr;
use std::time::Duration;

fn build_test_app() -> Router {
    Router::new()
        .route("/boards", get(boards::boards_page))
        .route("/api/ide/boards", get(boards::list_boards))
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

/// The `/boards` route is registered and serves a self-contained HTML page
/// that fetches the boards data endpoint.
#[tokio::test]
async fn boards_route_serves_html_page() {
    let addr = spawn_test_server().await;

    let resp = fbuild_core::http::client_with_timeout(Duration::from_secs(10))
        .get(format!("http://{}/boards", addr))
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .expect("GET /boards should not drop the connection");

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
    assert!(body.contains("/api/ide/boards"));
    assert!(body.contains("<title>fbuild Board Manager</title>"));
}

/// The `/api/ide/boards` route is registered and returns JSON.
#[tokio::test]
async fn api_ide_boards_route_returns_json_list() {
    let addr = spawn_test_server().await;

    let resp = fbuild_core::http::client_with_timeout(Duration::from_secs(10))
        .get(format!("http://{}/api/ide/boards", addr))
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .expect("GET /api/ide/boards should not drop the connection");

    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.expect("body should be json");
    assert_eq!(body["success"], serde_json::Value::Bool(true));
    let boards = body["boards"].as_array().expect("boards should be array");
    assert!(!boards.is_empty());
}

/// `?query=` narrows the result set server-side.
#[tokio::test]
async fn api_ide_boards_route_filters_by_query() {
    let addr = spawn_test_server().await;

    let resp = fbuild_core::http::client_with_timeout(Duration::from_secs(10))
        .get(format!("http://{}/api/ide/boards?query=uno", addr))
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .expect("GET /api/ide/boards?query=uno should not drop the connection");

    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.expect("body should be json");
    let boards = body["boards"].as_array().expect("boards should be array");
    assert!(boards.iter().any(|b| b["id"] == "uno"));
}
