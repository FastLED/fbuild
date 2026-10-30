//! The PlatformIO registry's User-Agent gate, as rendered in download errors
//! (FastLED/fbuild#1783).
//!
//! Split out of `downloader_tests.rs`, which is at the workspace's 1000-LOC
//! limit; `downloader_disk_tests.rs` is the same pattern.

use super::*;

/// A 403 from the registry names the User-Agent gate, so the next operator to
/// hit it is not left reading a bare "HTTP 403 Forbidden" — which is exactly
/// what made the original board-matrix outage hard to diagnose.
#[test]
fn registry_403_error_points_at_the_user_agent_gate() {
    let url = "https://dl.registry.platformio.org/download/platformio/tool/\
               framework-arduino-avr/5.4.0/framework-arduino-avr-5.4.0.tar.gz";
    let rendered = DownloadAttemptError::HttpStatus(reqwest::StatusCode::FORBIDDEN, None)
        .into_fbuild_error(url)
        .to_string();
    assert!(rendered.contains("User-Agent"), "{rendered}");
    assert!(rendered.contains("PlatformIO/"), "{rendered}");
}

/// The hint is scoped: it must not editorialize on unrelated hosts, nor on
/// statuses that have nothing to do with the gate.
#[test]
fn registry_gate_hint_stays_scoped() {
    use reqwest::StatusCode as S;
    let registry = "https://dl.registry.platformio.org/download/x.tar.gz";
    let elsewhere = "https://github.com/FastLED/fbuild/releases/download/x.tar.gz";

    assert!(!registry_gate_hint(registry, S::FORBIDDEN).is_empty());
    assert!(registry_gate_hint(registry, S::NOT_FOUND).is_empty());
    assert!(registry_gate_hint(elsewhere, S::FORBIDDEN).is_empty());
}
