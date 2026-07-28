//! Integration test for `GET /libraries` and `GET /api/ide/libraries`
//! (FastLED/fbuild#1076 Phase 2).
//!
//! Mirrors `test_plotter_route.rs`: build a minimal `Router` wired exactly
//! like `main.rs`, spawn it on an ephemeral port, and assert both routes
//! round-trip with the expected content — catching route-registration
//! regressions without needing the full production binary.

use axum::Router;
use axum::routing::get;
use fbuild_daemon::handlers::libraries;
use std::net::SocketAddr;
use std::time::Duration;

fn build_test_app() -> Router {
    Router::new()
        .route("/libraries", get(libraries::libraries_page))
        .route("/api/ide/libraries", get(libraries::list_libraries))
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

/// The `/libraries` route is registered and serves a self-contained HTML
/// page that fetches the libraries data endpoint.
#[tokio::test]
async fn libraries_route_serves_html_page() {
    let addr = spawn_test_server().await;

    let resp = fbuild_core::http::client_with_timeout(Duration::from_secs(10))
        .get(format!("http://{}/libraries", addr))
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .expect("GET /libraries should not drop the connection");

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
    assert!(body.contains("/api/ide/libraries"));
    assert!(body.contains("<title>fbuild Library Manager</title>"));
}

/// `GET /api/ide/libraries` without `?project=` returns 400 with a
/// how-to pointing at the CLI opener.
#[tokio::test]
async fn api_ide_libraries_route_missing_project_returns_400() {
    let addr = spawn_test_server().await;

    let resp = fbuild_core::http::client_with_timeout(Duration::from_secs(10))
        .get(format!("http://{}/api/ide/libraries", addr))
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .expect("GET /api/ide/libraries should not drop the connection");

    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: serde_json::Value = resp.json().await.expect("body should be json");
    assert_eq!(body["success"], serde_json::Value::Bool(false));
    let error = body["error"].as_str().unwrap_or_default();
    assert!(error.contains("fbuild libraries"));
}

/// `GET /api/ide/libraries?project=<real project>&env=<env>` returns the
/// classified `lib_deps` list.
#[tokio::test]
async fn api_ide_libraries_route_happy_path() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("platformio.ini"),
        "[env:uno]\nplatform = atmelavr\nboard = uno\nframework = arduino\nlib_deps = FastLED\n",
    )
    .expect("write platformio.ini");

    let addr = spawn_test_server().await;
    let url = format!(
        "http://{}/api/ide/libraries?project={}&env=uno",
        addr,
        urlencoding_lite(&tmp.path().display().to_string())
    );

    let resp = fbuild_core::http::client_with_timeout(Duration::from_secs(10))
        .get(url)
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .expect("GET /api/ide/libraries should not drop the connection");

    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.expect("body should be json");
    assert_eq!(body["success"], serde_json::Value::Bool(true));
    let libs = body["libraries"].as_array().expect("libraries array");
    assert_eq!(libs.len(), 1);
    assert_eq!(libs[0]["name"], "FastLED");
    assert!(body["install_state_note"].as_str().is_some());
}

/// Minimal percent-encoding for the query value in this test only — avoids
/// pulling in a URL-encoding dependency just for a test fixture path.
fn urlencoding_lite(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' | b':' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}
