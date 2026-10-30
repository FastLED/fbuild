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

/// The host test compares the parsed hostname, not a substring. A mirror that
/// merely carries the registry's name in its path is a different server and
/// must not collect the hint; hostnames are case-insensitive per RFC 4343, so
/// an uppercase spelling must still collect it.
#[test]
fn registry_gate_hint_matches_the_host_not_the_substring() {
    use reqwest::StatusCode as S;

    for impostor in [
        "https://mirror.example/dl.registry.platformio.org/pkg.tar.gz",
        "https://dl.registry.platformio.org.evil.test/pkg.tar.gz",
        "https://evil.test/?q=dl.registry.platformio.org",
    ] {
        assert!(
            registry_gate_hint(impostor, S::FORBIDDEN).is_empty(),
            "the registry name in a path or sibling domain is not the gating host: {impostor}"
        );
    }

    for real in [
        "https://DL.REGISTRY.PLATFORMIO.ORG/download/x.tar.gz",
        "https://dl.registry.PlatformIO.org/download/x.tar.gz",
    ] {
        assert!(
            !registry_gate_hint(real, S::FORBIDDEN).is_empty(),
            "hostnames are case-insensitive: {real}"
        );
    }

    // Garbage in must not panic, and must not earn the hint.
    assert!(registry_gate_hint("not a url", S::FORBIDDEN).is_empty());
}
