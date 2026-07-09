//! Unit tests for the parent `daemon_client` module. Extracted to keep the
//! parent file under the 1000-LOC gate (see ci.yml LOC Gate workflow).

use super::{
    broker_refusal_is_fatal, daemon_cache_identity_error, should_restart_daemon, DaemonAcquisition,
    DaemonInfoResponse,
};
use running_process::broker::client::RefusalKind::{VersionBlocked, VersionUnsupported};

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
