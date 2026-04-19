//! Integration tests for the `POST /api/test-emu` endpoint.
//!
//! Regression coverage for issue #130: the CLI saw
//! `error sending request for url (.../api/test-emu)` while the daemon was
//! healthy. The root cause was that the `test_emu` handler did not hold an
//! `OperationGuard`, so the daemon's 30 s self-eviction loop classified the
//! daemon as "empty" during a long ESP32/QEMU build and forced graceful
//! shutdown mid-request.
//!
//! These tests assert:
//! 1. The route is registered and returns a structured error (non-500) for
//!    a missing project directory — the error path the user hits when the
//!    daemon crashes during a real test-emu is now testable without
//!    producing a real firmware build.
//! 2. While the handler is running, `operation_in_progress` is set so the
//!    self-eviction loop will not fire.

use axum::routing::post;
use axum::Router;
use fbuild_daemon::context::DaemonContext;
use fbuild_daemon::handlers::emulator;
use fbuild_daemon::models::TestEmuRequest;
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

/// Build an axum app wired to the `/api/test-emu` endpoint exactly like
/// `main.rs` does. Kept in one place so route-registration regressions are
/// caught here instead of silently diverging from production wiring.
fn build_test_app(ctx: Arc<DaemonContext>) -> Router {
    Router::new()
        .route("/api/test-emu", post(emulator::test_emu))
        .with_state(ctx)
}

fn make_test_context() -> Arc<DaemonContext> {
    let (tx, _rx) = tokio::sync::watch::channel(false);
    Arc::new(DaemonContext::new(
        0, // port unused; we bind our own listener below
        tx,
        "test".to_string(),
    ))
}

/// Spawn the test app on an OS-assigned port and return the bound address.
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

/// The endpoint is registered. A request with a non-existent project dir
/// must round-trip as a structured JSON error (HTTP 400 with
/// `success=false`, `exit_code=1`) — not a connection drop or a 404.
#[tokio::test]
async fn test_emu_endpoint_returns_structured_error_for_missing_project() {
    let ctx = make_test_context();
    let addr = spawn_test_server(ctx).await;

    let body = serde_json::json!({
        "project_dir": "C:/definitely/does/not/exist/at/this/path",
        "environment": "uno",
        "verbose": false,
    });

    let resp = reqwest::Client::new()
        .post(format!("http://{}/api/test-emu", addr))
        .json(&body)
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .expect("POST /api/test-emu should not drop the connection");

    // Regression for issue #130: must NOT be a 5xx crash / panic and must
    // NOT be 404 (missing route). BAD_REQUEST is the handler's chosen
    // error status for invalid input.
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::BAD_REQUEST,
        "expected 400 for missing project dir, got {}",
        resp.status()
    );

    // OperationResponse only derives Serialize (not Deserialize), so we
    // read the body as a JSON Value and assert against the same fields
    // the CLI consumes via daemon_client::OperationResponse.
    let body: serde_json::Value = resp.json().await.expect("body must deserialize as JSON");
    assert_eq!(
        body.get("success").and_then(|v| v.as_bool()),
        Some(false),
        "success must be false on missing project"
    );
    assert_eq!(
        body.get("exit_code").and_then(|v| v.as_i64()),
        Some(1),
        "exit_code must be non-zero on failure"
    );
    let message = body
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert!(
        message.contains("project directory does not exist"),
        "message should describe the error, got {:?}",
        message
    );
}

/// Core regression for issue #130: the `test_emu` handler MUST register
/// an `OperationGuard` so the daemon's self-eviction loop does not tear
/// down the connection during a long (>30 s) build + emulate run.
///
/// We can't observe the flag mid-handler on the missing-project path
/// (it returns synchronously with no await points), but we CAN prove
/// the guard was constructed by asserting the guard's side-effect on
/// `last_activity`: `OperationGuard::new` calls `ctx.touch_activity()`,
/// so if we artificially back-date `last_activity` before calling the
/// handler, a correctly-registered guard will pull it forward. Without
/// the guard, `last_activity` stays back-dated and this test fails.
#[tokio::test]
async fn test_emu_registers_operation_guard() {
    use axum::extract::State;
    use axum::Json;
    use std::time::Instant;

    let ctx = make_test_context();

    // Back-date `last_activity` by 60 s so we can unambiguously detect a
    // `touch_activity()` call from inside the handler. If the handler
    // did not construct an OperationGuard, idle_duration would still be
    // ~60 s after the call.
    {
        let mut last = ctx
            .last_activity
            .lock()
            .expect("last_activity should be lockable");
        *last = Instant::now() - Duration::from_secs(60);
    }
    assert!(
        ctx.idle_duration() >= Duration::from_secs(55),
        "sanity: back-dated idle_duration should report ≥55 s before handler runs"
    );

    let req = TestEmuRequest {
        project_dir: "C:/definitely/does/not/exist/at/this/path".to_string(),
        environment: Some("uno".to_string()),
        verbose: false,
        timeout: None,
        halt_on_error: None,
        halt_on_success: None,
        expect: None,
        emulator: None,
        show_timestamp: true,
        request_id: Some("test-op-flag".to_string()),
        caller_pid: None,
        caller_cwd: None,
        pio_env: Default::default(),
    };

    let ctx_clone = Arc::clone(&ctx);
    let (_status, Json(resp)) = emulator::test_emu(State(ctx_clone), Json(req)).await;
    assert!(!resp.success, "missing project dir must fail");

    // The OperationGuard's `ctx.touch_activity()` call must have pulled
    // `last_activity` forward. Without the guard (the bug), idle_duration
    // would still be ~60 s.
    assert!(
        ctx.idle_duration() < Duration::from_secs(5),
        "handler must register an OperationGuard — touch_activity should have run. \
         idle_duration is {:?}",
        ctx.idle_duration()
    );

    // The guard must be dropped by the time the response returns, so
    // the flag is cleared and self-eviction is not permanently blocked.
    assert!(
        !ctx.operation_in_progress.load(Ordering::Relaxed),
        "operation_in_progress must be cleared after handler returns"
    );
}
