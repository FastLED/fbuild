//! `POST /api/test-emu` must read a relative `project_dir` against the
//! caller's cwd, not the daemon's (FastLED/fbuild#1415).

use super::select::test_emu;
use crate::context::DaemonContext;
use crate::models::TestEmuRequest;
use axum::Json;
use axum::extract::State;
use std::sync::Arc;

fn request(project_dir: &str, caller_cwd: &std::path::Path) -> TestEmuRequest {
    serde_json::from_value(serde_json::json!({
        "project_dir": project_dir,
        "caller_cwd": caller_cwd.to_str().unwrap(),
    }))
    .unwrap()
}

#[tokio::test]
async fn relative_project_dir_resolves_against_caller_cwd() {
    let (shutdown_tx, _rx) = tokio::sync::watch::channel(false);
    let ctx = Arc::new(DaemonContext::new(0, shutdown_tx, "test".to_string()));
    let caller = tempfile::tempdir_in(fbuild_paths::temp_subdir("daemon-tests")).unwrap();
    // A platform no runner knows: reaching this error proves the handler read
    // the caller's platformio.ini rather than the daemon cwd's (which has none).
    std::fs::write(
        caller.path().join("platformio.ini"),
        "[env:x]\nplatform = bogus-platform-1415\nboard = x\n",
    )
    .unwrap();

    let (_, Json(resp)) = test_emu(State(ctx), Json(request(".", caller.path()))).await;

    assert!(!resp.success);
    assert!(
        resp.message.contains("bogus-platform-1415"),
        "handler did not read the caller's project: {}",
        resp.message
    );
}

#[tokio::test]
async fn missing_relative_project_dir_reports_the_resolved_path() {
    let (shutdown_tx, _rx) = tokio::sync::watch::channel(false);
    let ctx = Arc::new(DaemonContext::new(0, shutdown_tx, "test".to_string()));
    let caller = tempfile::tempdir_in(fbuild_paths::temp_subdir("daemon-tests")).unwrap();

    let (_, Json(resp)) = test_emu(State(ctx), Json(request("nope", caller.path()))).await;

    assert!(!resp.success);
    let expected = caller.path().join("nope");
    assert!(
        resp.message.contains(&expected.display().to_string()),
        "message should name the resolved path {}: {}",
        expected.display(),
        resp.message
    );
}
