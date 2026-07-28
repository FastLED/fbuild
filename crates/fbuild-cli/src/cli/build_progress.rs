//! `fbuild build-progress`: open the daemon-served Build Progress web page
//! (FastLED/fbuild#1076 Phase 2, second panel) in the default browser.
//!
//! The page itself is a self-contained page served by fbuild-daemon at
//! `GET /build-progress` that polls the existing `/api/daemon/info`
//! endpoint for status and attaches to the existing `/ws/logs` broadcast
//! WebSocket for a live activity tail
//! (`crates/fbuild-daemon/web/build-progress/index.html`). So this
//! command's only job is: make sure the daemon is running, build the URL,
//! and hand it to the OS's default browser via
//! [`super::build::open_in_browser`] — the same helper `fbuild plotter`
//! uses.
//!
//! Deliberately a standalone tiny command (mirrors `cli::plotter`) rather
//! than an OS-specific "open a URL" invocation baked into
//! `.zed/tasks.json`: the generated Zed task (`fbuild ide`'s
//! `build_fbuild_tasks`) just runs `fbuild build-progress`, and the same
//! command works for anyone not using Zed.

use crate::daemon_client;
use crate::output;

use super::build::open_in_browser;

/// Build the `/build-progress` URL for the daemon at `base_url`. Pure —
/// no I/O — so it's directly testable without a running daemon.
pub fn build_progress_url(base_url: &str) -> String {
    format!("{base_url}/build-progress")
}

/// `fbuild build-progress`
pub async fn run_build_progress() -> fbuild_core::Result<()> {
    daemon_client::ensure_daemon_running().await?;
    let base_url = fbuild_paths::get_daemon_url();
    let url = build_progress_url(&base_url);

    output::progress(format!("Opening Build Progress: {}", url));
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
    fn build_progress_url_appends_path() {
        assert_eq!(
            build_progress_url("http://127.0.0.1:49200"),
            "http://127.0.0.1:49200/build-progress"
        );
    }
}
