//! Daemon exit paths: refuse new work once shutdown starts, a bounded exit on
//! SIGTERM, and the persist-and-clean-up step every clean exit runs.
//!
//! Before this module a SIGTERM on Linux/macOS (`fbuild daemon kill`, the
//! escalation in `fbuild daemon stop`, `docker stop`, CI teardown) killed the
//! daemon outright: no zccache flush, stale pid/port files. Routing SIGTERM
//! through the graceful HTTP drain is not an option either, because that
//! drain waits for every open request and a build is one long request.
//!
//! SIGTERM instead gets a controlled exit: new operations are refused at
//! once, in-flight ones get [`SHUTDOWN_DRAIN_BUDGET`] to finish, then the
//! zccache state is flushed (bounded by [`EXIT_FLUSH_BUDGET`]) and the process
//! exits. Build children still running at that point are reaped by the
//! process containment group when the daemon exits.

use crate::context::DaemonContext;
use crate::models::OperationResponse;
use axum::Json;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use fbuild_core::daemon_health::{EXIT_FLUSH_BUDGET, SHUTDOWN_DRAIN_BUDGET};
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// Message returned to operations refused because the daemon is stopping.
pub const SHUTTING_DOWN_MESSAGE: &str = "fbuild daemon is shutting down and accepts no new work; rerun the command to start a fresh daemon";

/// Middleware for operation routes (build, deploy, ...): once shutdown has
/// started, answer `503` with a failed [`OperationResponse`] instead of
/// starting work the exiting daemon would abandon.
pub async fn refuse_new_operations_when_shutting_down(
    State(ctx): State<Arc<DaemonContext>>,
    request: Request,
    next: Next,
) -> Response {
    if ctx.is_shutting_down.load(Ordering::Acquire) {
        tracing::info!(path = %request.uri().path(), "refused operation: daemon is shutting down");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(OperationResponse::fail(
                String::new(),
                SHUTTING_DOWN_MESSAGE.to_string(),
            )),
        )
            .into_response();
    }
    next.run(request).await
}

/// Resolve when the process receives SIGTERM. Never resolves on Windows,
/// where close/logoff/shutdown events go through the console handler
/// (`register_daemon_shutdown_handler`) instead.
pub async fn terminate_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut sigterm) => {
                sigterm.recv().await;
                return;
            }
            Err(error) => {
                tracing::warn!(
                    "cannot install SIGTERM handler ({error}); SIGTERM will kill the daemon without a flush"
                );
            }
        }
    }
    std::future::pending::<()>().await
}

/// Controlled exit on SIGTERM: refuse new operations, give in-flight ones
/// [`SHUTDOWN_DRAIN_BUDGET`], persist, exit.
pub async fn exit_on_terminate(ctx: Arc<DaemonContext>) -> ! {
    ctx.is_shutting_down.store(true, Ordering::Release);
    let in_flight = ctx.active_operations.load(Ordering::Acquire);
    tracing::info!(
        in_flight,
        "SIGTERM received: refusing new operations, waiting up to {}s for in-flight ones",
        SHUTDOWN_DRAIN_BUDGET.as_secs()
    );
    if ctx.wait_for_operations(SHUTDOWN_DRAIN_BUDGET).await {
        tracing::info!("in-flight operations finished");
    } else {
        tracing::warn!(
            remaining = ctx.active_operations.load(Ordering::Acquire),
            "in-flight operations still running after {}s; exiting anyway",
            SHUTDOWN_DRAIN_BUDGET.as_secs()
        );
    }
    persist_and_clean_up().await;
    tracing::info!("daemon exiting (SIGTERM)");
    std::process::exit(0)
}

/// Remove this daemon's pid/port/claim/status records and flush the embedded
/// zccache backend. Every clean exit runs this before `process::exit`.
pub async fn persist_and_clean_up() {
    let _ = fbuild_core::fs::remove_file(&fbuild_paths::get_daemon_pid_file()).await;
    let _ = fbuild_core::fs::remove_file(&fbuild_paths::get_daemon_port_file()).await;
    fbuild_paths::daemon_ownership::remove_owner_claim();
    // ...and the status file, which was previously left behind on every clean
    // shutdown, so `daemon status` kept reporting a dead PID (#1213 part 2).
    let _ = fbuild_core::fs::remove_file(&fbuild_paths::get_daemon_status_file()).await;

    // FastLED/fbuild#1480: `process::exit` runs no destructors and the
    // backend lives in a `OnceLock`, so without this flush zccache never
    // persisted `metadata.bin` (or the latest depgraph/index) and every
    // restart began cold (zackees/zccache#1652). Bounded so a slow flush
    // never outlasts the budgets of `fbuild daemon stop` / `kill` or the 10 s
    // a replacement daemon waits for the root-ownership lock.
    if let Some(backend) = fbuild_build::compile_backend::get_global() {
        match tokio::time::timeout(EXIT_FLUSH_BUDGET, backend.service().flush()).await {
            Ok(Ok(())) => tracing::info!("zccache backend flushed"),
            Ok(Err(err)) => tracing::warn!("zccache backend flush on exit failed: {err}"),
            Err(_) => tracing::warn!(
                "zccache backend flush on exit timed out after {}s",
                EXIT_FLUSH_BUDGET.as_secs()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::routing::post;
    use std::time::{Duration, Instant};

    fn context() -> Arc<DaemonContext> {
        let (shutdown_tx, _shutdown_rx) = tokio::sync::watch::channel(false);
        Arc::new(DaemonContext::new(0, shutdown_tx, ".".to_string()))
    }

    async fn serve_gated(ctx: Arc<DaemonContext>) -> String {
        let app = Router::new()
            .route("/api/build", post(|| async { "built" }))
            .route_layer(axum::middleware::from_fn_with_state(
                Arc::clone(&ctx),
                refuse_new_operations_when_shutting_down,
            ))
            .with_state(ctx);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/api/build", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await });
        url
    }

    #[tokio::test]
    async fn operations_run_while_the_daemon_is_up() {
        let url = serve_gated(context()).await;
        let resp = fbuild_core::http::client().post(&url).send().await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.text().await.unwrap(), "built");
    }

    #[tokio::test]
    async fn operations_are_refused_once_shutdown_starts() {
        let ctx = context();
        let url = serve_gated(Arc::clone(&ctx)).await;
        ctx.is_shutting_down.store(true, Ordering::Release);

        let resp = fbuild_core::http::client().post(&url).send().await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["success"], false);
        assert_eq!(body["message"], SHUTTING_DOWN_MESSAGE);
    }

    #[tokio::test]
    async fn drain_returns_as_soon_as_the_last_operation_ends() {
        let ctx = context();
        ctx.active_operations.store(2, Ordering::Release);
        let finisher = Arc::clone(&ctx);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            finisher.active_operations.fetch_sub(1, Ordering::AcqRel);
            tokio::time::sleep(Duration::from_millis(100)).await;
            finisher.active_operations.fetch_sub(1, Ordering::AcqRel);
        });
        let started = Instant::now();
        assert!(ctx.wait_for_operations(Duration::from_secs(5)).await);
        let waited = started.elapsed();
        assert!(waited >= Duration::from_millis(200), "{waited:?}");
        assert!(waited < Duration::from_secs(2), "{waited:?}");
    }

    #[tokio::test]
    async fn drain_gives_up_at_its_budget() {
        let ctx = context();
        ctx.active_operations.store(1, Ordering::Release);
        let started = Instant::now();
        assert!(!ctx.wait_for_operations(Duration::from_millis(200)).await);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn terminate_exit_budget_covers_drain_and_flush() {
        assert!(
            fbuild_core::daemon_health::TERMINATE_EXIT_BUDGET
                >= SHUTDOWN_DRAIN_BUDGET + EXIT_FLUSH_BUDGET
        );
    }
}
