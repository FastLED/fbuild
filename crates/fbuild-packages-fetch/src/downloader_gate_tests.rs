//! The PlatformIO registry's User-Agent gate, as rendered in download errors
//! (FastLED/fbuild#1783).
//!
//! Split out of `downloader_tests.rs`, which is at the workspace's 1000-LOC
//! limit; `downloader_disk_tests.rs` is the same pattern.

use super::*;

const REGISTRY_URL: &str = "https://dl.registry.platformio.org/download/platformio/tool/\
                            framework-arduino-avr/5.4.0/framework-arduino-avr-5.4.0.tar.gz";

/// A 403 from the registry names the User-Agent gate, so the next operator to
/// hit it is not left reading a bare "HTTP 403 Forbidden" — which is exactly
/// what made the original board-matrix outage hard to diagnose.
#[test]
fn registry_403_error_points_at_the_user_agent_gate() {
    let rendered = DownloadAttemptError::HttpStatus(
        reqwest::StatusCode::FORBIDDEN,
        None,
        Some("dl.registry.platformio.org".to_string()),
    )
    .into_fbuild_error(REGISTRY_URL)
    .to_string();
    assert!(rendered.contains("User-Agent"), "{rendered}");
    assert!(rendered.contains("PlatformIO/"), "{rendered}");
}

/// The hint is scoped: it must not editorialize on statuses that have nothing
/// to do with the gate, nor on hosts that are not the gating registry.
#[test]
fn registry_gate_hint_stays_scoped() {
    use reqwest::StatusCode as S;
    let registry = Some("dl.registry.platformio.org");

    assert!(!registry_gate_hint(registry, S::FORBIDDEN).is_empty());
    assert!(registry_gate_hint(registry, S::NOT_FOUND).is_empty());
    assert!(registry_gate_hint(Some("github.com"), S::FORBIDDEN).is_empty());
    assert!(registry_gate_hint(None, S::FORBIDDEN).is_empty());
}

/// The hint follows the host that *answered*, not the one we asked.
///
/// reqwest follows redirects by default and the registry 302s to a CDN, so a
/// 403 from that CDN (an expired signed URL, say) must not be blamed on
/// fbuild's User-Agent even though the requested URL still names the registry.
#[test]
fn a_cdn_403_after_a_redirect_is_not_blamed_on_the_user_agent() {
    let rendered = DownloadAttemptError::HttpStatus(
        reqwest::StatusCode::FORBIDDEN,
        None,
        Some("usc1.contabostorage.com".to_string()),
    )
    .into_fbuild_error(REGISTRY_URL)
    .to_string();

    assert!(rendered.contains("HTTP 403"), "{rendered}");
    assert!(
        !rendered.contains("User-Agent"),
        "a CDN's 403 is not the registry's UA gate: {rendered}"
    );
}

/// The host is compared whole, not as a substring: a server that merely
/// carries the registry's name inside its own hostname is a different server.
#[test]
fn registry_gate_hint_matches_the_host_not_a_substring() {
    use reqwest::StatusCode as S;
    for impostor in [
        "dl.registry.platformio.org.evil.test",
        "mirror.example",
        "notdl.registry.platformio.org",
    ] {
        assert!(
            registry_gate_hint(Some(impostor), S::FORBIDDEN).is_empty(),
            "{impostor} is not the gating host"
        );
    }
}

/// DNS hostnames are case-insensitive (RFC 4343), so a registry that answers
/// under a different spelling still collects the hint.
#[test]
fn registry_gate_hint_ignores_host_case() {
    use reqwest::StatusCode as S;
    for spelling in [
        "DL.REGISTRY.PLATFORMIO.ORG",
        "dl.registry.PlatformIO.org",
        "Dl.Registry.Platformio.Org",
    ] {
        assert!(
            !registry_gate_hint(Some(spelling), S::FORBIDDEN).is_empty(),
            "hostnames are case-insensitive: {spelling}"
        );
    }
}
