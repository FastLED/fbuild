//! Build Progress web page (FastLED/fbuild#1076 Phase 2, second panel): a
//! daemon-served, self-contained HTML page that shows the daemon's current
//! build/deploy state and a live tail of daemon log activity.
//!
//! ## Observability reality (read before changing this file)
//!
//! Actual build output (compiler invocation lines, one NDJSON `log` event
//! per line) streams over `POST /api/build`'s response body
//! (`crates/fbuild-daemon/src/handlers/operations/build.rs`). That channel
//! is **strictly per-request**: the log lines flow through an `unbounded`
//! channel created fresh for that one HTTP request and forwarded straight
//! into that request's own streaming response body. There is no fan-out —
//! a second client (like this page) cannot attach to another client's
//! in-flight build and see its compiler output line-by-line, and adding
//! that would mean either buffering full compile logs server-side or
//! wiring a new broadcast channel through the build orchestrator, neither
//! of which is "reuse an existing cheap broadcast".
//!
//! What *is* already broadcast to every subscriber, cheaply, via the
//! existing [`crate::context::BroadcastHub`] (`ws_logs` /
//! `ws_status` in `handlers/websockets.rs`):
//!
//! - `/ws/logs` — every `tracing::*` event the daemon emits process-wide
//!   (`BroadcastLogLayer`, wired in `main.rs`), including the build
//!   handler's own lifecycle tracing (project-lock wait/acquire, client
//!   disconnect/cancel, hard-deadline aborts, dependency-install
//!   messages) and every other subsystem's events (deploy, esptool
//!   write-flash progress, etc). It is not literal compiler stdout, but
//!   it is a real-time, multi-subscriber view of what the daemon is
//!   doing.
//! - `/api/daemon/info` (polled here every ~2s) and `/ws/status` (push,
//!   not used here to keep this page's contract identical to a plain
//!   poll loop) — `daemon_state`, `current_operation`,
//!   `operation_in_progress`, `dependency_install`.
//!
//! So this page is built entirely out of **existing, unmodified**
//! endpoints: it polls `/api/daemon/info` for the status header and
//! attaches to `/ws/logs` for the scrolling log pane. No new daemon
//! endpoint or broadcast channel was added — per FastLED/fbuild#1076
//! Phase 2's guidance to prefer reusing an existing broadcast channel
//! over inventing new server-side state.

use axum::response::{Html, IntoResponse};

const BUILD_PROGRESS_PAGE_HTML: &str = include_str!("../../web/build-progress/index.html");

/// GET /build-progress — serve the self-contained Build Progress page.
pub async fn build_progress_page() -> impl IntoResponse {
    Html(BUILD_PROGRESS_PAGE_HTML)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn build_progress_page_serves_html_with_no_external_deps() {
        let response = build_progress_page().await.into_response();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let content_type = response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        assert!(
            content_type.starts_with("text/html"),
            "expected text/html content type, got {content_type}"
        );

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should be readable");
        let html = String::from_utf8(body.to_vec()).expect("body should be utf-8");

        assert!(
            html.contains("/api/daemon/info"),
            "build progress page must poll the existing daemon info endpoint for status"
        );
        assert!(
            html.contains("/ws/logs"),
            "build progress page must attach to the existing broadcast log websocket"
        );
        assert!(
            !html.contains("cdn.")
                && !html.contains("unpkg.com")
                && !html.contains("jsdelivr.net")
                && !html.contains("googleapis.com"),
            "build progress page must be self-contained with no CDN dependencies"
        );
    }
}
