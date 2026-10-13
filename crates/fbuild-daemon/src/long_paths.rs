//! Windows long-path policy at the HTTP operation boundary.

use crate::context::DaemonContext;
use crate::models::OperationResponse;
use axum::Json;
use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use fbuild_paths::long_paths::Observation;
use futures::StreamExt;
use std::sync::Arc;

/// Refusal returned when a registry change requires fresh daemon processes.
pub const SETTING_CHANGED_MESSAGE: &str = "WARNING: Windows long-path support changed after this daemon started. The daemon is retiring after existing operations finish. Reconnect or rerun the command to start a fresh daemon; run fbuild --fix-win to enable long paths.";

/// Read-only preflight and advisory delivery for package/build operations.
/// Apply outside the shutdown admission gate so a changed setting retires
/// the daemon before admitting work that would inherit its cached policy.
pub async fn operation_policy(
    State(ctx): State<Arc<DaemonContext>>,
    request: Request,
    next: Next,
) -> Response {
    if !is_package_operation(request.uri().path()) {
        return next.run(request).await;
    }
    apply_operation_policy(
        &ctx,
        ctx.long_path_support.as_ref(),
        &fbuild_paths::long_paths::probe(),
        ctx.long_path_advisory.as_deref(),
        request,
        next,
    )
    .await
}

async fn apply_operation_policy(
    ctx: &DaemonContext,
    startup: Option<&Observation>,
    fresh: &Observation,
    advisory: Option<&str>,
    request: Request,
    next: Next,
) -> Response {
    if !is_package_operation(request.uri().path()) {
        return next.run(request).await;
    }

    if startup.is_some_and(|observation| observation.status_key() != fresh.status_key()) {
        // Close admission atomically before telling axum to drain its existing
        // requests. `force` here means accepting retirement while busy; it
        // neither cancels operations nor kills child processes.
        let in_flight = ctx.try_begin_shutdown(true);
        tracing::warn!(
            ?startup,
            ?fresh,
            ?in_flight,
            "retiring daemon after Windows long-path policy change"
        );
        let _ = ctx.shutdown_tx.send(true);
        let mut response = OperationResponse::fail(String::new(), SETTING_CHANGED_MESSAGE.into());
        response.stderr = Some(SETTING_CHANGED_MESSAGE.into());
        return (StatusCode::SERVICE_UNAVAILABLE, Json(response)).into_response();
    }

    let response = next.run(request).await;
    match advisory {
        Some(message) => attach_advisory(response, message).await,
        None => response,
    }
}

fn is_package_operation(path: &str) -> bool {
    matches!(
        path,
        "/api/build" | "/api/deploy" | "/api/test-emu" | "/api/install-deps"
    )
}

/// Preserve tool diagnostics and the original status while adding plain
/// warning text. Streamed bodies remain lazy, so a warning reaches the
/// caller even while package resolution has not produced its first log.
async fn attach_advisory(response: Response, advisory: &str) -> Response {
    let warning = if advisory.starts_with("WARNING:") {
        advisory.to_string()
    } else {
        format!("WARNING: {advisory}")
    };
    let (mut parts, body) = response.into_parts();
    let content_type = parts
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim();
    let is_stream = content_type == "application/x-ndjson";
    let is_json = content_type == "application/json";
    // The warning changes body length; the HTTP stack computes it anew.
    parts.headers.remove(header::CONTENT_LENGTH);

    if is_stream {
        let event = serde_json::json!({"type": "log", "message": warning});
        return Response::from_parts(
            parts,
            prepend_body(body, bytes::Bytes::from(format!("{event}\n"))),
        );
    }

    if !is_json {
        // Axum extractor errors are plain text. Keep their diagnostics and
        // status intact rather than trying to deserialize an operation model.
        return Response::from_parts(
            parts,
            prepend_body(body, bytes::Bytes::from(format!("{warning}\n"))),
        );
    }

    let original = match to_bytes(body, usize::MAX).await {
        Ok(bytes) => bytes,
        Err(error) => {
            let mut response = OperationResponse::fail(
                String::new(),
                format!("could not read operation response: {error}\n{warning}"),
            );
            response.stderr = Some(warning);
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(response)).into_response();
        }
    };
    match serde_json::from_slice::<serde_json::Value>(&original) {
        Ok(mut document) if document.is_object() => {
            let stderr = document.get("stderr").and_then(|value| value.as_str());
            let stderr = match stderr.filter(|diagnostics| !diagnostics.is_empty()) {
                Some(diagnostics) => format!("{warning}\n{diagnostics}"),
                None => warning.clone(),
            };
            let message = document.get("message").and_then(|value| value.as_str());
            let message = match message.filter(|message| !message.is_empty()) {
                Some(message) => format!("{message}\n{warning}"),
                None => warning,
            };
            document["stderr"] = serde_json::Value::String(stderr);
            document["message"] = serde_json::Value::String(message);
            Response::from_parts(parts, Body::from(document.to_string()))
        }
        _ => Response::from_parts(
            parts,
            prepend_body(
                Body::from(original),
                bytes::Bytes::from(format!("{warning}\n")),
            ),
        ),
    }
}

fn prepend_body(body: Body, prefix: bytes::Bytes) -> Body {
    let first = futures::stream::once(std::future::ready(Ok::<_, axum::Error>(prefix)));
    Body::from_stream(first.chain(body.into_data_stream()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::{emulator, health, operations, websockets};
    use crate::models::OperationResponse;
    use axum::body::Body;
    use axum::extract::State;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use futures::StreamExt;
    use std::sync::Arc;
    use std::time::Duration;

    const WARNING: &str = "WARNING: Using short-path fallback cache. Run fbuild --fix-win to enable Windows long paths.";

    #[derive(Clone)]
    struct Policy {
        ctx: Arc<DaemonContext>,
        startup: Option<Observation>,
        fresh: Observation,
        advisory: Option<String>,
    }

    async fn fixture_policy(
        State(policy): State<Policy>,
        request: Request,
        next: Next,
    ) -> Response {
        apply_operation_policy(
            &policy.ctx,
            policy.startup.as_ref(),
            &policy.fresh,
            policy.advisory.as_deref(),
            request,
            next,
        )
        .await
    }

    fn context() -> (Arc<DaemonContext>, tokio::sync::watch::Receiver<bool>) {
        let (tx, rx) = tokio::sync::watch::channel(false);
        (
            Arc::new(DaemonContext::new(0, tx, "long-path-http-test".into())),
            rx,
        )
    }

    fn disabled_policy(ctx: Arc<DaemonContext>) -> Policy {
        Policy {
            ctx,
            startup: Some(Observation::Disabled),
            fresh: Observation::Disabled,
            advisory: Some(WARNING.into()),
        }
    }

    async fn serve(app: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    async fn outcome() -> Response {
        let mut response = OperationResponse::ok("fixture".into(), "Build succeeded".into());
        response.stdout = Some("compiler output".into());
        response.stderr = Some("existing compiler diagnostic".into());
        Json(response).into_response()
    }

    #[tokio::test]
    async fn fallback_advisory_preserves_success_and_original_diagnostics() {
        let (ctx, _rx) = context();
        let app = Router::new()
            .route("/api/build", post(outcome))
            .route_layer(axum::middleware::from_fn_with_state(
                disabled_policy(ctx),
                fixture_policy,
            ));
        let response = fbuild_core::http::client()
            .post(format!("{}/api/build", serve(app).await))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value = response.json().await.unwrap();
        assert_eq!(body["success"], true);
        assert_eq!(body["exit_code"], 0);
        assert_eq!(body["stdout"], "compiler output");
        assert_eq!(
            body["stderr"],
            format!("{WARNING}\nexisting compiler diagnostic")
        );
        assert_eq!(body["message"], format!("Build succeeded\n{WARNING}"));
        assert!(!body.to_string().contains("\\u001b"));
    }

    #[tokio::test]
    async fn all_package_endpoints_preserve_advisory_on_early_validation_failure() {
        let (ctx, _rx) = context();
        let app = Router::new()
            .route("/api/build", post(operations::build))
            .route("/api/deploy", post(operations::deploy))
            .route("/api/test-emu", post(emulator::test_emu))
            .route("/api/install-deps", post(operations::install_deps))
            .route_layer(axum::middleware::from_fn_with_state(
                disabled_policy(Arc::clone(&ctx)),
                fixture_policy,
            ))
            .with_state(ctx);
        let base = serve(app).await;
        let fixture = tempfile::tempdir().unwrap();
        let missing_project = fixture.path().join("missing-project");
        for endpoint in ["build", "deploy", "test-emu", "install-deps"] {
            let response = fbuild_core::http::client()
                .post(format!("{base}/api/{endpoint}"))
                .json(&serde_json::json!({"project_dir": missing_project, "stream": true}))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{endpoint}");
            let body: serde_json::Value = response.json().await.unwrap();
            assert_eq!(body["success"], false, "{endpoint}");
            assert_eq!(body["stderr"], WARNING, "{endpoint}");
            assert!(
                body["message"].as_str().unwrap().contains(WARNING),
                "{endpoint}"
            );
            assert!(
                body["message"].as_str().unwrap().contains("does not exist"),
                "{endpoint}"
            );
        }
    }

    #[tokio::test]
    async fn streaming_advisory_precedes_pending_body_and_keeps_terminal_result() {
        let (ctx, _rx) = context();
        let release = Arc::new(tokio::sync::Notify::new());
        let release_handler = Arc::clone(&release);
        let app = Router::new()
            .route("/api/build", post(move || {
                let release = Arc::clone(&release_handler);
                async move {
                    let stream = futures::stream::once(async move {
                        release.notified().await;
                        Ok::<_, std::convert::Infallible>(bytes::Bytes::from_static(
                            b"{\"type\":\"log\",\"message\":\"package error\"}\n{\"type\":\"result\",\"success\":false,\"exit_code\":1}\n",
                        ))
                    });
                    ([("content-type", "application/x-ndjson")], Body::from_stream(stream))
                }
            }))
            .route_layer(axum::middleware::from_fn_with_state(disabled_policy(ctx), fixture_policy));
        let response = fbuild_core::http::client()
            .post(format!("{}/api/build", serve(app).await))
            .send()
            .await
            .unwrap();
        let mut chunks = response.bytes_stream();
        let first = tokio::time::timeout(Duration::from_secs(2), chunks.next())
            .await
            .expect("warning must be available while the operation body is pending")
            .unwrap()
            .unwrap();
        let event: serde_json::Value = serde_json::from_slice(&first).unwrap();
        assert_eq!(event["type"], "log");
        assert_eq!(event["message"], WARNING);
        release.notify_one();
        let mut remainder = Vec::new();
        while let Some(chunk) = chunks.next().await {
            remainder.extend(chunk.unwrap());
        }
        let remainder = String::from_utf8(remainder).unwrap();
        assert!(remainder.contains("package error"));
        assert!(remainder.contains("\"type\":\"result\""));
        assert!(!remainder.contains(WARNING));
    }

    #[tokio::test]
    async fn setting_change_refuses_work_and_retires_without_interrupting_active_operation() {
        let (ctx, rx) = context();
        let admission = ctx.begin_operation_admission().unwrap();
        let operation = operations::OperationGuard::new(
            &ctx,
            fbuild_core::DaemonState::Building,
            Some("existing build".into()),
        );
        let mut policy = disabled_policy(Arc::clone(&ctx));
        policy.fresh = Observation::Enabled;
        let app = Router::new()
            .route("/api/build", post(outcome))
            .route_layer(axum::middleware::from_fn_with_state(
                Arc::clone(&ctx),
                crate::shutdown::refuse_new_operations_when_shutting_down,
            ))
            .route_layer(axum::middleware::from_fn_with_state(policy, fixture_policy));
        let response = fbuild_core::http::client()
            .post(format!("{}/api/build", serve(app).await))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body: serde_json::Value = response.json().await.unwrap();
        assert_eq!(body["success"], false);
        assert_eq!(body["message"], SETTING_CHANGED_MESSAGE);
        assert_eq!(body["stderr"], SETTING_CHANGED_MESSAGE);
        assert!(*rx.borrow());
        assert!(ctx.begin_operation_admission().is_none());
        assert_eq!(ctx.operation_drain_count(), 2);
        assert!(!ctx.wait_for_operations(Duration::ZERO).await);
        drop(admission);
        assert_eq!(ctx.operation_drain_count(), 1);
        assert!(!ctx.wait_for_operations(Duration::ZERO).await);
        drop(operation);
        assert!(ctx.wait_for_operations(Duration::ZERO).await);
    }

    #[tokio::test]
    async fn changing_unknown_diagnostic_keeps_same_daemon() {
        let (ctx, rx) = context();
        let mut policy = disabled_policy(ctx);
        policy.startup = Some(Observation::Unknown("access denied".into()));
        policy.fresh = Observation::Unknown("query failed".into());
        let app = Router::new()
            .route("/api/build", post(outcome))
            .route_layer(axum::middleware::from_fn_with_state(policy, fixture_policy));
        let response = fbuild_core::http::client()
            .post(format!("{}/api/build", serve(app).await))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(!*rx.borrow());
    }

    #[tokio::test]
    async fn non_package_commands_ignore_fallback_and_setting_changes() {
        let (ctx, rx) = context();
        let mut policy = disabled_policy(ctx);
        policy.fresh = Observation::Enabled;
        let app = Router::new()
            .route("/api/reset", post(outcome))
            .route_layer(axum::middleware::from_fn_with_state(policy, fixture_policy));
        let response = fbuild_core::http::client()
            .post(format!("{}/api/reset", serve(app).await))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = response.json().await.unwrap();
        assert_eq!(body["stderr"], "existing compiler diagnostic");
        assert!(!*rx.borrow());
    }

    #[tokio::test]
    async fn enabled_policy_preserves_response_without_advisory() {
        let (ctx, _rx) = context();
        let policy = Policy {
            ctx,
            startup: Some(Observation::Enabled),
            fresh: Observation::Enabled,
            advisory: None,
        };
        let app = Router::new()
            .route("/api/build", post(outcome))
            .route_layer(axum::middleware::from_fn_with_state(policy, fixture_policy));
        let response = fbuild_core::http::client()
            .post(format!("{}/api/build", serve(app).await))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = response.json().await.unwrap();
        assert_eq!(body["message"], "Build succeeded");
        assert_eq!(body["stderr"], "existing compiler diagnostic");
    }

    #[tokio::test]
    async fn daemon_info_reports_startup_observation_for_client_handoff() {
        let (ctx, _rx) = context();
        let mut ctx = Arc::try_unwrap(ctx).ok().unwrap();
        ctx.long_path_support = Some(Observation::Disabled);
        let app = Router::new()
            .route("/api/daemon/info", get(health::daemon_info))
            .with_state(Arc::new(ctx));
        let response = fbuild_core::http::client()
            .get(format!("{}/api/daemon/info", serve(app).await))
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = response.json().await.unwrap();
        assert_eq!(
            body["long_path_support"],
            serde_json::json!({"status":"disabled"})
        );
    }

    #[tokio::test]
    async fn extractor_rejection_keeps_plain_warning_and_parse_error() {
        let (ctx, _rx) = context();
        let app = Router::new()
            .route("/api/build", post(operations::build))
            .route_layer(axum::middleware::from_fn_with_state(
                disabled_policy(Arc::clone(&ctx)),
                fixture_policy,
            ))
            .with_state(ctx);
        let response = fbuild_core::http::client()
            .post(format!("{}/api/build", serve(app).await))
            .header("content-type", "application/json")
            .body("{")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = response.text().await.unwrap();
        assert!(body.starts_with(WARNING));
        assert!(body.contains("Failed to parse"));
    }

    async fn serve_draining_build(
        ctx: Arc<DaemonContext>,
        mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
        started: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    ) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
        let handler_ctx = Arc::clone(&ctx);
        let handler_started = Arc::clone(&started);
        let handler_release = Arc::clone(&release);
        let app = Router::new()
            .route(
                "/api/build",
                post(move || {
                    let ctx = Arc::clone(&handler_ctx);
                    let started = Arc::clone(&handler_started);
                    let release = Arc::clone(&handler_release);
                    async move {
                        let _operation = operations::OperationGuard::new(
                            &ctx,
                            fbuild_core::DaemonState::Building,
                            Some("admitted build".into()),
                        );
                        started.notify_one();
                        release.notified().await;
                        Json(OperationResponse::ok(
                            "drain-fixture".into(),
                            "Build finished".into(),
                        ))
                    }
                }),
            )
            .route_layer(axum::middleware::from_fn_with_state(
                Arc::clone(&ctx),
                crate::shutdown::refuse_new_operations_when_shutting_down,
            ))
            .route("/ws/serial-monitor", get(websockets::ws_serial_monitor))
            .route(
                "/ws/monitor/:session_id",
                get(websockets::ws_monitor_session),
            )
            .route("/ws/status", get(websockets::ws_status))
            .route("/ws/logs", get(websockets::ws_logs))
            .with_state(Arc::clone(&ctx));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.changed().await;
                })
                .await
                .unwrap();
        });
        (address, server)
    }

    #[tokio::test]
    async fn retirement_drains_admitted_build_without_waiting_for_idle_websockets() {
        let (ctx, shutdown_rx) = context();
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let (address, mut server) = serve_draining_build(
            Arc::clone(&ctx),
            shutdown_rx,
            Arc::clone(&started),
            Arc::clone(&release),
        )
        .await;

        let mut sockets = Vec::new();
        for endpoint in ["serial-monitor", "monitor/drain-fixture", "status", "logs"] {
            let (socket, _) =
                tokio_tungstenite::connect_async(format!("ws://{address}/ws/{endpoint}"))
                    .await
                    .unwrap();
            sockets.push(socket);
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            while ctx
                .pending_serial_attaches
                .load(std::sync::atomic::Ordering::Acquire)
                == 0
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("real serial WebSocket must be waiting for its attach frame");

        let build = tokio::spawn(async move {
            fbuild_core::http::client()
                .post(format!("http://{address}/api/build"))
                .send()
                .await
                .unwrap()
                .json::<serde_json::Value>()
                .await
                .unwrap()
        });
        tokio::time::timeout(Duration::from_secs(2), started.notified())
            .await
            .unwrap();
        ctx.begin_shutdown();
        ctx.shutdown_tx.send(true).unwrap();
        assert!(ctx.begin_operation_admission().is_none());
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut server)
                .await
                .is_err(),
            "retirement must wait for the admitted build"
        );

        release.notify_one();
        let result = tokio::time::timeout(Duration::from_secs(2), build)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result["success"], true);
        assert_eq!(result["message"], "Build finished");
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .expect("idle upgraded WebSockets must not hold the HTTP drain")
            .unwrap();
        assert_eq!(ctx.operation_drain_count(), 0);
        assert_eq!(
            ctx.pending_serial_attaches
                .load(std::sync::atomic::Ordering::Acquire),
            1,
            "the upgraded serial handler is detached from the completed HTTP server"
        );
        for mut socket in sockets {
            let _ = socket.close(None).await;
        }
    }
}
