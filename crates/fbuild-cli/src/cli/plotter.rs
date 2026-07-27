//! `fbuild plotter`: open the daemon-served Serial Plotter web page
//! (FastLED/fbuild#1076 Phase 2) in the default browser.
//!
//! The plotter itself is a self-contained page served by fbuild-daemon at
//! `GET /plotter` that connects to the existing `/ws/serial-monitor`
//! WebSocket and populates its own port selector from `/api/devices/list`
//! (`crates/fbuild-daemon/web/plotter/index.html`). So this command's only
//! job is: make sure the daemon is running, build the URL (optionally
//! pinning `?port=<port>` so the page auto-attaches instead of requiring a
//! manual pick), and hand it to the OS's default browser via
//! [`super::build::open_in_browser`] — the same helper the avr8js emulator
//! path (`cli::deploy`) already uses.
//!
//! This is deliberately a standalone tiny command rather than encoding an
//! OS-specific "open a URL" invocation inside `.zed/tasks.json`: the
//! generated Zed task (`fbuild ide`'s `build_fbuild_tasks`) just runs
//! `fbuild plotter`, and the same command works for anyone not using Zed.

use crate::daemon_client;
use crate::output;

use super::build::open_in_browser;

/// Build the `/plotter` URL for the daemon at `base_url`, optionally
/// pinning a port via the `?port=` query param. Pure — no I/O — so it's
/// directly testable without a running daemon.
pub fn plotter_url(base_url: &str, port: Option<&str>) -> String {
    match port {
        Some(p) => format!("{base_url}/plotter?port={}", percent_encode_query(p)),
        None => format!("{base_url}/plotter"),
    }
}

/// Minimal percent-encoding for the handful of characters that show up in
/// a serial port name (`COM3`, `/dev/ttyUSB0`, ...) but aren't safe unescaped
/// in a URL query value. Deliberately small — not a general-purpose encoder.
fn percent_encode_query(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// `fbuild plotter [--port <port>]`
pub async fn run_plotter(port: Option<String>) -> fbuild_core::Result<()> {
    daemon_client::ensure_daemon_running().await?;
    let base_url = fbuild_paths::get_daemon_url();
    let url = plotter_url(&base_url, port.as_deref());

    output::progress(format!("Opening Serial Plotter: {}", url));
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
    fn plotter_url_without_port() {
        assert_eq!(
            plotter_url("http://127.0.0.1:49200", None),
            "http://127.0.0.1:49200/plotter"
        );
    }

    #[test]
    fn plotter_url_with_com_port_is_unescaped() {
        assert_eq!(
            plotter_url("http://127.0.0.1:49200", Some("COM3")),
            "http://127.0.0.1:49200/plotter?port=COM3"
        );
    }

    #[test]
    fn plotter_url_with_unix_device_path_is_percent_encoded() {
        assert_eq!(
            plotter_url("http://127.0.0.1:49200", Some("/dev/ttyUSB0")),
            "http://127.0.0.1:49200/plotter?port=%2Fdev%2FttyUSB0"
        );
    }

    #[test]
    fn percent_encode_query_leaves_safe_chars_alone() {
        assert_eq!(percent_encode_query("abc-DEF_123.~"), "abc-DEF_123.~");
    }
}
