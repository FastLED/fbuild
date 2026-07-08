use std::path::{Path, PathBuf};

use super::{DaemonClient, DaemonInfoResponse};

pub async fn warn_if_daemon_identity_mismatch(client: &DaemonClient, project_dir: &str) {
    match client.daemon_info().await {
        Ok(info) => {
            tracing::debug!(
                daemon_version = %info.version,
                daemon_port = info.port,
                daemon_spawner_cwd = info.spawner_cwd.as_deref().unwrap_or("unknown"),
                daemon_cache_identity = info.cache_identity.as_deref().unwrap_or("unknown"),
                project_dir,
                "serving daemon identity"
            );
            if let Some(warning) = daemon_identity_warning(&info, project_dir) {
                crate::output::warn(warning);
            }
        }
        Err(err) => {
            tracing::debug!("failed to query daemon identity: {}", err);
        }
    }
}

pub fn daemon_identity_warning(info: &DaemonInfoResponse, project_dir: &str) -> Option<String> {
    let cli_version = env!("CARGO_PKG_VERSION");
    let version_mismatch = info.version != cli_version;
    let checkout_mismatch = info
        .spawner_cwd
        .as_deref()
        .filter(|cwd| known_spawner_cwd(cwd))
        .is_some_and(|cwd| !same_checkout_scope(cwd, project_dir));
    let cache_mismatch = daemon_cache_identity_warning(info);

    if !version_mismatch && !checkout_mismatch && cache_mismatch.is_none() {
        return None;
    }

    let daemon_cwd = info
        .spawner_cwd
        .as_deref()
        .filter(|cwd| known_spawner_cwd(cwd))
        .unwrap_or("unknown checkout");
    let mut reasons = Vec::new();
    if version_mismatch {
        reasons.push(format!(
            "version mismatch: daemon={}, cli={}",
            info.version, cli_version
        ));
    }
    if checkout_mismatch {
        reasons.push("checkout mismatch".to_string());
    }
    if let Some(reason) = cache_mismatch {
        reasons.push(reason);
    }

    Some(format!(
        "daemon identity warning: serving daemon at 127.0.0.1:{} is fbuild {} spawned by {}; this CLI is fbuild {} for {} ({})",
        info.port,
        info.version,
        daemon_cwd,
        cli_version,
        project_dir,
        reasons.join(", ")
    ))
}

fn daemon_cache_identity_warning(info: &DaemonInfoResponse) -> Option<String> {
    let expected_identity = fbuild_paths::running_process::DaemonCacheIdentity::discover();
    let expected_label = expected_identity.label_value();
    if info.cache_identity.as_deref() != Some(expected_label.as_str()) {
        return Some(format!(
            "cache identity mismatch: daemon={:?}, cli={}",
            info.cache_identity.as_deref(),
            expected_label
        ));
    }

    let expected_schema = fbuild_paths::running_process::CACHE_SCHEMA_VERSION;
    if info.cache_schema_version != Some(expected_schema) {
        return Some(format!(
            "cache schema mismatch: daemon={:?}, cli={}",
            info.cache_schema_version, expected_schema
        ));
    }

    None
}

fn known_spawner_cwd(cwd: &str) -> bool {
    let trimmed = cwd.trim();
    !trimmed.is_empty() && !trimmed.eq_ignore_ascii_case("unknown")
}

fn same_checkout_scope(spawner_cwd: &str, project_dir: &str) -> bool {
    let spawner = identity_path(spawner_cwd);
    let project = identity_path(project_dir);
    spawner == project
        || path_is_ancestor(&spawner, &project)
        || path_is_ancestor(&project, &spawner)
}

fn identity_path(path: &str) -> String {
    let path = Path::new(path);
    let absolute = if path.is_absolute() {
        PathBuf::from(path)
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| PathBuf::from(path))
    };
    let canonical = std::fs::canonicalize(&absolute).unwrap_or(absolute);
    let mut normalized = canonical.to_string_lossy().replace('\\', "/");
    while normalized.ends_with('/') && normalized.len() > 1 {
        normalized.pop();
    }
    if cfg!(windows) {
        normalized.to_ascii_lowercase()
    } else {
        normalized
    }
}

fn path_is_ancestor(parent: &str, child: &str) -> bool {
    child
        .strip_prefix(parent)
        .is_some_and(|rest| rest.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(version: &str, spawner_cwd: Option<&str>) -> DaemonInfoResponse {
        let identity = fbuild_paths::running_process::DaemonCacheIdentity::discover();
        DaemonInfoResponse {
            status: "running".to_string(),
            uptime_seconds: 1.0,
            version: version.to_string(),
            pid: 123,
            port: 8765,
            dev_mode: fbuild_paths::is_dev_mode(),
            operation_in_progress: false,
            daemon_state: fbuild_core::DaemonState::Idle,
            current_operation: None,
            dependency_install: None,
            client_count: 0,
            cache_identity: Some(identity.label_value()),
            cache_schema_version: Some(fbuild_paths::running_process::CACHE_SCHEMA_VERSION),
            spawner_cwd: spawner_cwd.map(str::to_string),
            source_mtime: None,
        }
    }

    #[test]
    fn daemon_identity_warning_detects_version_mismatch() {
        let warning = daemon_identity_warning(&info("0.0.0", None), ".");

        let warning = warning.expect("version mismatch should warn");
        assert!(warning.contains("version mismatch"));
        assert!(warning.contains("daemon=0.0.0"));
    }

    #[test]
    fn daemon_identity_warning_detects_checkout_mismatch() {
        let warning = daemon_identity_warning(
            &info(env!("CARGO_PKG_VERSION"), Some("C:/work/fastled6")),
            "C:/work/fastled3/.build/pio/lpc845brk",
        );

        let warning = warning.expect("different checkout should warn");
        assert!(warning.contains("checkout mismatch"));
        assert!(warning.contains("fastled6"));
        assert!(warning.contains("fastled3"));
    }

    #[test]
    fn daemon_identity_warning_accepts_project_under_spawner_checkout() {
        let warning = daemon_identity_warning(
            &info(env!("CARGO_PKG_VERSION"), Some("C:/work/fastled3")),
            "C:/work/fastled3/.build/pio/lpc845brk",
        );

        assert!(warning.is_none());
    }

    #[test]
    fn daemon_identity_warning_ignores_unknown_spawner_when_version_matches() {
        let warning =
            daemon_identity_warning(&info(env!("CARGO_PKG_VERSION"), Some("unknown")), ".");

        assert!(warning.is_none());
    }

    #[test]
    fn daemon_identity_warning_detects_cache_identity_mismatch() {
        let mut info = info(env!("CARGO_PKG_VERSION"), None);
        info.cache_identity = Some("mode=other;cache=C:/other".to_string());

        let warning =
            daemon_identity_warning(&info, ".").expect("cache identity mismatch should warn");

        assert!(warning.contains("cache identity mismatch"));
        assert!(warning.contains("mode=other"));
    }
}
