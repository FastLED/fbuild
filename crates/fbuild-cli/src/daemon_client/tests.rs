//! Unit tests for the parent `daemon_client` module. Extracted to keep the
//! parent file under the 1000-LOC gate (see ci.yml LOC Gate workflow).

use super::{
    DaemonAcquisition, DaemonClient, DaemonInfoResponse, HealthResponseFull,
    broker_refusal_is_fatal, daemon_cache_identity_error, launcher_path,
    restart_diag::SiblingDaemon, same_running_image, should_restart_daemon, wedged_daemon_note,
};
use running_process::broker::client::RefusalKind::{VersionBlocked, VersionUnsupported};

#[test]
fn launcher_path_accepts_windows_spelling() {
    let path = launcher_path([
        ("UNRELATED".into(), "ignored".into()),
        ("Path".into(), "C:\\tools".into()),
    ])
    .expect("PATH is present");

    assert_eq!(path, "C:\\tools");
}

#[test]
fn broker_version_refusals_are_fatal() {
    assert!(broker_refusal_is_fatal(Some(VersionUnsupported)));
    assert!(broker_refusal_is_fatal(Some(VersionBlocked)));
}

#[test]
fn broker_non_refusal_errors_can_fallback() {
    assert!(!broker_refusal_is_fatal(None));
}

#[test]
fn broker_acquisition_reports_negotiated_state() {
    let acquisition = DaemonAcquisition::BrokerNegotiated {
        endpoint: "rp-backend".to_string(),
        daemon_version: Some("2.2.29".to_string()),
    };

    assert_eq!(acquisition.mode(), "broker-negotiated");
    assert_eq!(acquisition.endpoint(), Some("rp-backend"));
    assert_eq!(acquisition.daemon_version(), Some("2.2.29"));
    assert_eq!(acquisition.reason(), None);
    assert!(acquisition.summary().contains("version 2.2.29"));
}

#[test]
fn direct_acquisition_reports_fallback_reason() {
    let acquisition = DaemonAcquisition::DirectFallback {
        reason: "broker unavailable".to_string(),
    };

    assert_eq!(acquisition.mode(), "direct-fallback");
    assert_eq!(acquisition.endpoint(), None);
    assert_eq!(acquisition.daemon_version(), None);
    assert_eq!(acquisition.reason(), Some("broker unavailable"));
    assert!(acquisition.summary().contains("broker unavailable"));
}

fn daemon_info_for_cache_identity(
    cache_identity: Option<String>,
    cache_schema_version: Option<u32>,
) -> DaemonInfoResponse {
    DaemonInfoResponse {
        status: "running".to_string(),
        uptime_seconds: 1.0,
        version: "2.2.29".to_string(),
        pid: 123,
        port: 8765,
        dev_mode: fbuild_paths::is_dev_mode(),
        operation_in_progress: false,
        daemon_state: fbuild_core::DaemonState::Idle,
        current_operation: None,
        dependency_install: None,
        client_count: 0,
        cache_identity,
        cache_schema_version,
        spawner_cwd: None,
        source_mtime: None,
    }
}

#[test]
fn daemon_cache_identity_accepts_current_identity() {
    let identity = fbuild_paths::running_process::DaemonCacheIdentity::discover();
    let info = daemon_info_for_cache_identity(
        Some(identity.label_value()),
        Some(fbuild_paths::running_process::CACHE_SCHEMA_VERSION),
    );

    assert!(daemon_cache_identity_error(&info).is_none());
}

#[test]
fn daemon_cache_identity_rejects_missing_identity() {
    let info = daemon_info_for_cache_identity(
        None,
        Some(fbuild_paths::running_process::CACHE_SCHEMA_VERSION),
    );

    let err = daemon_cache_identity_error(&info).expect("missing identity must fail closed");
    assert!(err.contains("cache identity"));
}

#[test]
fn daemon_cache_identity_rejects_wrong_schema() {
    let identity = fbuild_paths::running_process::DaemonCacheIdentity::discover();
    let info = daemon_info_for_cache_identity(Some(identity.label_value()), Some(u32::MAX));

    let err = daemon_cache_identity_error(&info).expect("schema mismatch must fail closed");
    assert!(err.contains("cache schema"));
}

// FastLED/fbuild#1009 — version-based daemon arbitration.

#[test]
fn older_cli_never_evicts_newer_daemon_regardless_of_mtime() {
    // The bug: a freshly-built OLDER binary (newer mtime) displacing a running
    // NEWER daemon. Must not restart even though cli_mtime > daemon_mtime.
    assert!(!should_restart_daemon("2.4.0", "2.5.0", 9999.0, 1.0));
    assert!(!should_restart_daemon("2.4.0", "2.4.1", 9999.0, 1.0));
}

#[test]
fn newer_cli_upgrades_the_daemon() {
    // CLI strictly newer → restart regardless of mtime.
    assert!(should_restart_daemon("2.5.0", "2.4.0", 1.0, 9999.0));
    assert!(should_restart_daemon("2.4.1", "2.4.0", 0.0, 0.0));
}

#[test]
fn same_version_restarts_only_on_newer_binary_mtime() {
    // Dev rebuild of the same version: restart iff the on-disk binary is newer.
    assert!(should_restart_daemon("2.4.0", "2.4.0", 200.0, 100.0));
    assert!(!should_restart_daemon("2.4.0", "2.4.0", 100.0, 200.0));
    assert!(!should_restart_daemon("2.4.0", "2.4.0", 100.0, 100.0));
    // No usable mtimes → don't churn.
    assert!(!should_restart_daemon("2.4.0", "2.4.0", 0.0, 0.0));
}

#[test]
fn unparseable_versions_fall_back_to_mtime() {
    assert!(should_restart_daemon("not-semver", "2.4.0", 200.0, 100.0));
    assert!(!should_restart_daemon("2.4.0", "garbage", 100.0, 200.0));
}

#[tokio::test]
async fn content_check_resolves_newer_mtime_without_hiding_different_images() {
    use std::io::{Read, Write};

    let image = tempfile::NamedTempFile::new().unwrap();
    let memo_dir = tempfile::tempdir().unwrap();
    std::fs::write(image.path(), b"running image bytes").unwrap();
    let sibling = SiblingDaemon {
        path: Some(image.path().to_string_lossy().into_owned()),
        mtime: 200.0,
    };
    let health = HealthResponseFull {
        status: "healthy".into(),
        uptime_seconds: 1.0,
        version: "2.4.0".into(),
        pid: 42,
        source_mtime: 100.0,
        source_exe: None,
        launched_by_broker: None,
    };
    assert!(should_restart_daemon(
        "2.4.0",
        "2.4.0",
        sibling.mtime,
        health.source_mtime
    ));

    for (remote_pid, remote_bytes, expected_match) in [
        (42, b"running image bytes".as_slice(), true),
        (42, b"different image bytes".as_slice(), false),
        (99, b"running image bytes".as_slice(), false),
    ] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let digest = blake3::hash(remote_bytes).to_hex().to_string();
        let response = format!(r#"{{"pid":{remote_pid},"blake3":"{digest}"}}"#);
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let count = stream.read(&mut request).unwrap();
            assert!(String::from_utf8_lossy(&request[..count]).contains("/api/daemon/image-hash"));
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response.len(),
                response
            );
            stream.write_all(reply.as_bytes()).unwrap();
        });
        let client = DaemonClient::with_port(port);
        assert_eq!(
            same_running_image(&client, &health, &sibling, memo_dir.path()).await,
            expected_match
        );
        server.join().unwrap();
    }
}

/// FastLED/fbuild#1360: when a daemon is alive but not answering, the spawn
/// failure must say so and name the recovery, rather than leaving the caller
/// with an error that reads like their sketch failed to compile.
#[test]
fn a_live_unresponsive_daemon_is_named_along_with_its_recovery() {
    let note = wedged_daemon_note(Some(4321));
    assert!(note.contains("4321"), "{note}");
    assert!(
        note.contains("fbuild daemon stop"),
        "the hint must point at the recovery, not just describe the problem: {note}"
    );
}

/// Without a live daemon of our own there is nothing to diagnose — and a guess
/// here would be worse than silence, because the spawn failure already carries
/// a correct version-mismatch explanation that the hint would talk over.
#[test]
fn no_live_daemon_produces_no_hint() {
    assert_eq!(wedged_daemon_note(None), "");
}
