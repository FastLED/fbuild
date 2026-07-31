//! Board Manager panel (FastLED/fbuild#1076 Phase 2): a daemon-served,
//! self-contained, **read-only** HTML page plus its data endpoint for
//! browsing fbuild's embedded board database.
//!
//! - `GET /api/ide/boards?query=<optional filter>` — list boards from
//!   [`fbuild_config::search_boards`] (id, name, platform, mcu, and
//!   f_cpu/ram/flash where the registry entry carries them), with an
//!   optional case-insensitive substring filter on id/name.
//! - `GET /boards` — the page itself; fetches the endpoint above and
//!   renders a filterable, expandable table client-side. No mutation, no
//!   network fetches, no external assets.

use axum::Json;
use axum::extract::Query;
use axum::response::{Html, IntoResponse};

use crate::models::{IdeBoardsQuery, IdeBoardsResponse};

const BOARDS_PAGE_HTML: &str = include_str!("../../web/boards/index.html");

/// GET /boards — serve the self-contained Board Manager page.
pub async fn boards_page() -> impl IntoResponse {
    Html(BOARDS_PAGE_HTML)
}

/// `GET /api/ide/boards?query=<optional filter>`
pub async fn list_boards(Query(params): Query<IdeBoardsQuery>) -> Json<IdeBoardsResponse> {
    let boards = fbuild_config::search_boards(params.query.as_deref());
    Json(IdeBoardsResponse {
        success: true,
        boards,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn boards_page_serves_html_pointing_at_the_api_with_no_external_deps() {
        let response = boards_page().await.into_response();
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
            html.contains("/api/ide/boards"),
            "boards page must fetch the boards data endpoint"
        );
        assert!(
            !html.contains("cdn.")
                && !html.contains("unpkg.com")
                && !html.contains("jsdelivr.net")
                && !html.contains("googleapis.com"),
            "boards page must be self-contained with no CDN dependencies"
        );
    }

    #[tokio::test]
    async fn list_boards_with_no_query_returns_every_board() {
        let response = list_boards(Query(IdeBoardsQuery { query: None })).await;
        assert!(response.success);
        assert!(!response.boards.is_empty());
    }

    #[tokio::test]
    async fn list_boards_filters_by_query() {
        let all = list_boards(Query(IdeBoardsQuery { query: None })).await;
        let response = list_boards(Query(IdeBoardsQuery {
            query: Some("uno".to_string()),
        }))
        .await;
        assert!(response.success);
        assert!(response.boards.iter().any(|b| b.id == "uno"));
        assert!(
            response.boards.len() < all.boards.len(),
            "filter should narrow results"
        );
    }

    #[tokio::test]
    async fn list_boards_unmatched_query_returns_empty_but_success() {
        let response = list_boards(Query(IdeBoardsQuery {
            query: Some("definitely-not-a-real-board-xyz".to_string()),
        }))
        .await;
        assert!(response.success);
        assert!(response.boards.is_empty());
    }
}
