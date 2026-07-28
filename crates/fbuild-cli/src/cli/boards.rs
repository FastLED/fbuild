//! `fbuild boards`: open the daemon-served Board Manager web page
//! (FastLED/fbuild#1076 Phase 2) in the default browser.
//!
//! The page itself is a self-contained, **read-only** page served by
//! fbuild-daemon at `GET /boards` that fetches
//! `GET /api/ide/boards?query=<filter>` (backed by
//! `fbuild_config::search_boards`, the embedded board database also used
//! by `fbuild build`/`fbuild deploy`) and renders a filterable table
//! client-side (`crates/fbuild-daemon/web/boards/index.html`). Unlike
//! `fbuild libraries`, this page is daemon-global — it needs no project
//! context — so this command's only job is: make sure the daemon is
//! running, build the URL, and hand it to the OS's default browser via
//! [`super::build::open_in_browser`] — the same helper `fbuild plotter`
//! and `fbuild build-progress` use.

use crate::daemon_client;
use crate::output;

use super::build::open_in_browser;

/// Build the `/boards` URL for the daemon at `base_url`. Pure — no I/O —
/// so it's directly testable without a running daemon.
pub fn boards_url(base_url: &str) -> String {
    format!("{base_url}/boards")
}

/// `fbuild boards`
pub async fn run_boards() -> fbuild_core::Result<()> {
    daemon_client::ensure_daemon_running().await?;
    let base_url = fbuild_paths::get_daemon_url();
    let url = boards_url(&base_url);

    output::progress(format!("Opening Board Manager: {}", url));
    if let Err(e) = open_in_browser(&url).await {
        output::warn(format!("failed to open browser: {}", e));
        output::warn(format!("open this URL manually: {}", url));
    }
    output::result(url);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boards_url_appends_path() {
        assert_eq!(
            boards_url("http://127.0.0.1:49200"),
            "http://127.0.0.1:49200/boards"
        );
    }
}
