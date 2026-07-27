//! Serial Plotter web page (FastLED/fbuild#1076 Phase 2): a daemon-served,
//! self-contained HTML page that connects to the existing
//! `/ws/serial-monitor` WebSocket and renders a live scrolling line chart
//! of numeric values parsed from serial output, Arduino-Serial-Plotter
//! style.
//!
//! Unlike the avr8js emulator pages (`super::emulator::avr8js_web`), the
//! plotter has no server-side session state — it only needs a serial port
//! that's already open or openable, which `/ws/serial-monitor`'s existing
//! `Attach { open_if_needed: true, .. }` handshake already provides, and a
//! port list, which `POST /api/devices/list` already provides. All parsing
//! and rendering is client-side JS; the page has no build step and no
//! external dependencies (CSP-safe, no CDN — same embedding pattern as
//! `avr8js_web::AVR8JS_APP_JS`).

use axum::response::{Html, IntoResponse};

const PLOTTER_PAGE_HTML: &str = include_str!("../../web/plotter/index.html");

/// GET /plotter — serve the self-contained Serial Plotter page.
pub async fn plotter_page() -> impl IntoResponse {
    Html(PLOTTER_PAGE_HTML)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn plotter_page_serves_html_containing_ws_endpoint_and_no_external_deps() {
        let response = plotter_page().await.into_response();
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
            html.contains("/ws/serial-monitor"),
            "plotter page must connect to the existing serial monitor websocket"
        );
        assert!(
            html.contains("/api/devices/list"),
            "plotter page must populate its port selector from the devices endpoint"
        );
        assert!(
            !html.contains("cdn.")
                && !html.contains("unpkg.com")
                && !html.contains("jsdelivr.net")
                && !html.contains("googleapis.com"),
            "plotter page must be self-contained with no CDN dependencies"
        );
    }
}
