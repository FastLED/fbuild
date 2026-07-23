//! Standalone project/framework cleanup command.

use crate::daemon_client::{self, BuildRequest, DaemonClient};
use crate::output;
use fbuild_core::process_identity::{pid_exe_stem_matches, pid_is_alive, terminate_pid, wait_for_pid_exit};
use fbuild_paths::daemon_ownership::{self, DAEMON_EXE_STEM, RootOwnershipGuard, SpawnLockGuard};
use std::future::Future;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use super::args::{CleanScope, DaemonAction};
use super::daemon_cmd::run_daemon;
use super::deploy::print_operation_streams;

pub async fn run_clean(
    project_dir: String,
    environment: Option<String>,
    scope: CleanScope,
    quick: bool,
    release: bool,
) -> fbuild_core::Result<()> {
    if matches!(scope, CleanScope::Cache) {
        reset_compiler_cache().await?;
    }
    let profile = if release {
        Some("release".to_string())
    } else if quick {
        Some("quick".to_string())
    } else {
        None
    };
    let (caller_pid, caller_cwd) = daemon_client::caller_info();
    let client = DaemonClient::new();
    daemon_client::warn_if_daemon_identity_mismatch(&client, &project_dir).await;
    let req = BuildRequest {
        project_dir,
        environment,
        clean_build: true,
        clean_all: matches!(scope, CleanScope::All | CleanScope::Cache),
        clean_only: true,
        verbose: false,
        jobs: None,
        profile,
        generate_compiledb: false,
        compiledb_only: false,
        request_id: None,
        caller_pid,
        caller_cwd,
        stream: true,
        symbol_analysis: false,
        symbol_analysis_path: None,
        no_timestamp: false,
        src_dir: std::env::var("PLATFORMIO_SRC_DIR")
            .ok()
            .filter(|value| !value.is_empty()),
        output_dir: None,
        pio_env: daemon_client::capture_pio_env(),
        bloat_analysis: false,
    };

    let response = client.build_streaming(&req).await?;
    print_operation_streams(&response);
    if !response.success {
        output::error(response.message);
        std::process::exit(response.exit_code);
    }
    Ok(())
}

/// Reset the global compiler cache (`<fbuild_root>/zccache`).
///
/// soldr-style ownership orchestration (FastLED/fbuild#1159): a spawn-herd
/// single-flight lock is held across the whole reset so no concurrent daemon
/// spawner races the deletion; a legacy/rollout sweep hunts down every other
/// live `fbuild-daemon` that might still hold the cache root (via HTTP for
/// discoverable daemons and verified-PID signalling for stale/legacy ones);
/// and the exclusive `RootOwnershipGuard` is the final, positive proof that
/// every owning daemon has actually exited before the directory is removed.
async fn reset_compiler_cache() -> fbuild_core::Result<()> {
    let compiler_cache = fbuild_paths::get_fbuild_root().join("zccache");

    // 1) Spawn-herd single flight, held across the whole reset.
    let spawn_guard = acquire_reset_spawn_lock().await;

    let client = DaemonClient::new();

    // 2) Identity validation: refuse if the daemon at the resolved port
    // belongs to a different mode/cache root.
    validate_running_daemon_identity(&client).await?;

    // 3) Graceful stop of the current-version daemon.
    let stop_result = run_daemon(DaemonAction::Stop).await;
    if let Err(stop_error) = stop_result {
        let daemon_stayed_healthy = daemon_stays_healthy(&client).await;
        accept_stop_outcome(Err(stop_error), daemon_stayed_healthy)?;
    }

    // 4) Legacy/rollout sweep: discover and shut down every other live
    // fbuild-daemon that could own this cache root.
    if let Err(sweep_error) = sweep_legacy_daemons().await {
        drop(spawn_guard);
        let restore_result = daemon_client::ensure_daemon_running().await;
        return combine_reset_results(Err(sweep_error), restore_result);
    }

    // 5) Exclusive root ownership: the positive proof every owner exited.
    let ownership_guard = match acquire_root_ownership_before_reset().await {
        Ok(guard) => guard,
        Err(ownership_error) => {
            drop(spawn_guard);
            let restore_result = daemon_client::ensure_daemon_running().await;
            return combine_reset_results(Err(ownership_error), restore_result);
        }
    };

    // 6) Delete the cache.
    let cleanup_result = remove_compiler_cache_at(&compiler_cache).await;

    // 7) Drop ownership + spawn guards *before* restarting so the new daemon
    // (and its own spawn-lock acquisition) isn't blocked by our own guards.
    drop(ownership_guard);
    drop(spawn_guard);
    finish_cache_reset(cleanup_result, || run_daemon(DaemonAction::Restart)).await?;

    output::result(format!(
        "global compiler cache cleared: {}",
        compiler_cache.display()
    ));
    Ok(())
}

/// Acquire the spawn-herd single-flight lock, retrying briefly if another
/// process is currently mid-spawn. Root ownership (step 5) is the final
/// gate, so failing to acquire this lock after the retry budget is not
/// fatal — the reset proceeds anyway.
async fn acquire_reset_spawn_lock() -> Option<SpawnLockGuard> {
    const RETRIES: usize = 50;
    const POLL_INTERVAL: Duration = Duration::from_millis(100);

    for attempt in 0..RETRIES {
        if let Some(guard) = daemon_ownership::try_acquire_spawn_lock() {
            return Some(guard);
        }
        if attempt + 1 < RETRIES {
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
    None
}

async fn validate_running_daemon_identity(client: &DaemonClient) -> fbuild_core::Result<()> {
    if !client.health().await {
        return Ok(());
    }

    let info = client.daemon_info().await?;
    if let Some(error) = daemon_client::daemon_cache_identity_error(&info) {
        return Err(fbuild_core::FbuildError::DaemonError(format!(
            "refusing compiler cache reset: {error}"
        )));
    }
    Ok(())
}

async fn daemon_stays_healthy(client: &DaemonClient) -> bool {
    const CHECKS: usize = 20;
    const POLL_INTERVAL: Duration = Duration::from_millis(100);

    for check in 0..CHECKS {
        if !client.health().await {
            return false;
        }
        if check + 1 < CHECKS {
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
    true
}

fn accept_stop_outcome(
    stop_result: fbuild_core::Result<()>,
    daemon_stayed_healthy: bool,
) -> fbuild_core::Result<()> {
    match stop_result {
        Ok(()) => Ok(()),
        Err(_) if !daemon_stayed_healthy => Ok(()),
        Err(error) => Err(error),
    }
}

// --- Legacy/rollout sweep (FastLED/fbuild#1159 step 4) ---

/// Discover and shut down every other live `fbuild-daemon` that could own
/// this cache root: any daemon reachable via a `daemon-*.port` file whose
/// reported cache identity matches ours (HTTP shutdown), then any PID
/// recorded via the owner claim, the stable legacy PID file, or
/// `daemon_status.json` (verified-PID signal, never an unverified/recycled
/// PID).
async fn sweep_legacy_daemons() -> fbuild_core::Result<()> {
    shutdown_matching_port_daemons().await?;

    let mut candidates: Vec<u32> = Vec::new();
    if let Some(claim) = daemon_ownership::read_owner_claim() {
        candidates.push(claim.pid);
    }
    if let Some(pid) = read_stable_pid_file() {
        candidates.push(pid);
    }
    if let Some(pid) = read_status_json_daemon_pid() {
        candidates.push(pid);
    }
    candidates.sort_unstable();
    candidates.dedup();

    for pid in candidates {
        terminate_legacy_pid_if_verified(pid).await;
    }
    Ok(())
}

/// Enumerate `daemon-*.port` files under `get_daemon_dir()`. For each port
/// that answers `/api/daemon/info` with a matching cache identity, request a
/// graceful (non-force) shutdown. A busy daemon (still healthy after the
/// shutdown request errors) is a hard refusal of the whole reset. A daemon
/// reporting a *different* cache identity is left alone.
async fn shutdown_matching_port_daemons() -> fbuild_core::Result<()> {
    let daemon_dir = fbuild_paths::get_daemon_dir();
    let entries = match std::fs::read_dir(&daemon_dir) {
        Ok(entries) => entries,
        Err(_) => return Ok(()),
    };

    let expected_identity = fbuild_paths::running_process::DaemonCacheIdentity::discover();
    let expected_label = expected_identity.label_value();

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !is_daemon_port_file_name(file_name) {
            continue;
        }
        let Some(port) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|contents| parse_port_file_contents(&contents))
        else {
            continue;
        };

        let legacy_client = DaemonClient::with_port(port);
        let Ok(info) = legacy_client.daemon_info().await else {
            // Did not answer: not live, or already stopped. Nothing to do.
            continue;
        };
        if info.cache_identity.as_deref() != Some(expected_label.as_str()) {
            // Different cache identity: leave it alone.
            continue;
        }

        if let Err(shutdown_error) = legacy_client.shutdown().await {
            let still_healthy = daemon_stays_healthy(&legacy_client).await;
            legacy_shutdown_outcome(port, shutdown_error, still_healthy)?;
        }
    }
    Ok(())
}

/// Decide the outcome of a failed legacy-daemon shutdown request: a daemon
/// still healthy after the request errored is busy and refuses the whole
/// reset; a daemon that is gone (connection lost because it actually shut
/// down) is accepted.
fn legacy_shutdown_outcome(
    port: u16,
    shutdown_error: fbuild_core::FbuildError,
    still_healthy: bool,
) -> fbuild_core::Result<()> {
    if still_healthy {
        Err(fbuild_core::FbuildError::DaemonError(format!(
            "refusing compiler cache reset: legacy daemon on port {port} refused shutdown ({shutdown_error})"
        )))
    } else {
        Ok(())
    }
}

fn is_daemon_port_file_name(name: &str) -> bool {
    name.starts_with("daemon-") && name.ends_with(".port")
}

fn parse_port_file_contents(contents: &str) -> Option<u16> {
    let port: u16 = contents.trim().parse().ok()?;
    (port > 0).then_some(port)
}

fn parse_stable_pid_contents(contents: &str) -> Option<u32> {
    contents.trim().parse().ok()
}

fn read_stable_pid_file() -> Option<u32> {
    std::fs::read_to_string(fbuild_paths::get_daemon_pid_file())
        .ok()
        .and_then(|contents| parse_stable_pid_contents(&contents))
}

fn parse_status_json_daemon_pid(contents: &str) -> Option<u32> {
    let value: serde_json::Value = serde_json::from_str(contents).ok()?;
    value.get("daemon_pid")?.as_u64().map(|pid| pid as u32)
}

fn read_status_json_daemon_pid() -> Option<u32> {
    std::fs::read_to_string(fbuild_paths::get_daemon_status_file())
        .ok()
        .and_then(|contents| parse_status_json_daemon_pid(&contents))
}

/// The verified-PID identity gate: only signal a PID that is alive AND
/// whose running executable stem matches `fbuild-daemon`. A recycled PID
/// running an unrelated program must never be signalled.
fn should_signal_legacy_pid(is_alive: bool, exe_stem_matches: bool) -> bool {
    is_alive && exe_stem_matches
}

async fn terminate_legacy_pid_if_verified(pid: u32) {
    let verified = should_signal_legacy_pid(pid_is_alive(pid), pid_exe_stem_matches(pid, DAEMON_EXE_STEM));
    if !verified {
        return;
    }
    if wait_for_pid_exit(pid, Duration::from_secs(5)) {
        return;
    }
    terminate_pid(pid);
    wait_for_pid_exit(pid, Duration::from_secs(5));
}

// --- Exclusive root ownership (FastLED/fbuild#1159 step 5) ---

async fn acquire_root_ownership_before_reset() -> fbuild_core::Result<RootOwnershipGuard> {
    const TIMEOUT: Duration = Duration::from_secs(30);
    const POLL_INTERVAL: Duration = Duration::from_millis(100);
    let started = Instant::now();

    loop {
        match RootOwnershipGuard::try_acquire() {
            Ok(Some(guard)) => return Ok(guard),
            Ok(None) => {}
            Err(error) => {
                return Err(fbuild_core::FbuildError::DaemonError(format!(
                    "failed while waiting for exclusive cache-root ownership: {error}"
                )));
            }
        }
        if started.elapsed() >= TIMEOUT {
            return Err(fbuild_core::FbuildError::DaemonError(
                "a live fbuild-daemon still owns the cache root; reset aborted".to_string(),
            ));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn finish_cache_reset<Restart, RestartFuture>(
    cleanup_result: fbuild_core::Result<()>,
    restart: Restart,
) -> fbuild_core::Result<()>
where
    Restart: FnOnce() -> RestartFuture,
    RestartFuture: Future<Output = fbuild_core::Result<()>>,
{
    let restart_result = restart().await;
    combine_reset_results(cleanup_result, restart_result)
}

fn combine_reset_results(
    cleanup_result: fbuild_core::Result<()>,
    restart_result: fbuild_core::Result<()>,
) -> fbuild_core::Result<()> {
    match (cleanup_result, restart_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(cleanup_error), Ok(())) => Err(cleanup_error),
        (Ok(()), Err(restart_error)) => Err(restart_error),
        (Err(cleanup_error), Err(restart_error)) => Err(fbuild_core::FbuildError::Other(format!(
            "compiler cache cleanup failed: {cleanup_error}; daemon restart also failed: {restart_error}"
        ))),
    }
}

async fn remove_compiler_cache_at(compiler_cache: &Path) -> fbuild_core::Result<()> {
    if compiler_cache.file_name() != Some(std::ffi::OsStr::new("zccache")) {
        return Err(fbuild_core::FbuildError::Other(format!(
            "refusing to remove unexpected compiler cache path: {}",
            compiler_cache.display()
        )));
    }

    const RETRIES: usize = 20;
    for attempt in 0..=RETRIES {
        match fbuild_core::fs::remove_dir_all(compiler_cache).await {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) if attempt < RETRIES => {
                tracing::debug!(
                    path = %compiler_cache.display(),
                    attempt = attempt + 1,
                    error = %error,
                    "compiler cache removal retry"
                );
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            Err(error) => return Err(error.into()),
        }
    }
    unreachable!("compiler cache retry loop always returns")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[tokio::test]
    async fn compiler_cache_cleanup_removes_only_requested_root_and_is_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let compiler_cache = temp.path().join("zccache");
        let sibling_cache = temp.path().join("cache");
        fbuild_core::fs::create_dir_all(compiler_cache.join("objects"))
            .await
            .unwrap();
        fbuild_core::fs::create_dir_all(&sibling_cache)
            .await
            .unwrap();
        fbuild_core::fs::write(compiler_cache.join("objects/cached.o"), b"object")
            .await
            .unwrap();
        fbuild_core::fs::write(sibling_cache.join("package.tar"), b"package")
            .await
            .unwrap();

        remove_compiler_cache_at(&compiler_cache).await.unwrap();
        remove_compiler_cache_at(&compiler_cache).await.unwrap();

        assert!(!compiler_cache.exists());
        assert!(sibling_cache.join("package.tar").is_file());
    }

    #[tokio::test]
    async fn cleanup_failure_still_attempts_daemon_restart() {
        let restart_calls = Arc::new(AtomicUsize::new(0));
        let observed_calls = Arc::clone(&restart_calls);

        let result = finish_cache_reset(
            Err(fbuild_core::FbuildError::Other(
                "synthetic cleanup failure".to_string(),
            )),
            move || {
                let observed_calls = Arc::clone(&observed_calls);
                async move {
                    observed_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            },
        )
        .await;

        assert!(result.is_err());
        assert_eq!(restart_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn daemon_stop_refusal_is_preserved_while_daemon_stays_healthy() {
        let result = accept_stop_outcome(
            Err(fbuild_core::FbuildError::DaemonError(
                "operation in progress".to_string(),
            )),
            true,
        );

        assert!(result.is_err());
    }

    #[test]
    fn lost_stop_response_is_accepted_after_daemon_exits() {
        let result = accept_stop_outcome(
            Err(fbuild_core::FbuildError::DaemonError(
                "connection closed".to_string(),
            )),
            false,
        );

        assert!(result.is_ok());
    }

    #[test]
    fn legacy_shutdown_busy_daemon_is_hard_refusal() {
        let result = legacy_shutdown_outcome(
            49200,
            fbuild_core::FbuildError::DaemonError("operation in progress".to_string()),
            true,
        );

        assert!(result.is_err());
    }

    #[test]
    fn legacy_shutdown_lost_connection_after_daemon_exit_is_accepted() {
        let result = legacy_shutdown_outcome(
            49200,
            fbuild_core::FbuildError::DaemonError("connection closed".to_string()),
            false,
        );

        assert!(result.is_ok());
    }

    #[test]
    fn is_daemon_port_file_name_matches_expected_pattern() {
        assert!(is_daemon_port_file_name("daemon-0123456789abcdef.port"));
        assert!(!is_daemon_port_file_name("daemon.log"));
        assert!(!is_daemon_port_file_name("daemon_status.json"));
        assert!(!is_daemon_port_file_name("fbuild_daemon.pid"));
    }

    #[test]
    fn parse_port_file_contents_rejects_garbage_and_zero() {
        assert_eq!(parse_port_file_contents("49200"), Some(49200));
        assert_eq!(parse_port_file_contents("49200\n"), Some(49200));
        assert_eq!(parse_port_file_contents("0"), None);
        assert_eq!(parse_port_file_contents("not-a-port"), None);
        assert_eq!(parse_port_file_contents(""), None);
    }

    #[test]
    fn parse_stable_pid_contents_parses_valid_and_rejects_garbage() {
        assert_eq!(parse_stable_pid_contents("1234"), Some(1234));
        assert_eq!(parse_stable_pid_contents("1234\n"), Some(1234));
        assert_eq!(parse_stable_pid_contents("not-a-pid"), None);
        assert_eq!(parse_stable_pid_contents(""), None);
    }

    #[test]
    fn parse_status_json_daemon_pid_round_trips_and_rejects_malformed() {
        assert_eq!(
            parse_status_json_daemon_pid(r#"{"daemon_pid": 4321}"#),
            Some(4321)
        );
        assert_eq!(parse_status_json_daemon_pid(r#"{"daemon_pid": null}"#), None);
        assert_eq!(parse_status_json_daemon_pid(r#"{}"#), None);
        assert_eq!(parse_status_json_daemon_pid("not json"), None);
    }

    #[test]
    fn should_signal_legacy_pid_requires_both_gates() {
        assert!(should_signal_legacy_pid(true, true));
        assert!(!should_signal_legacy_pid(true, false));
        assert!(!should_signal_legacy_pid(false, true));
        assert!(!should_signal_legacy_pid(false, false));
    }

    /// A recycled/unrelated PID must never be signalled: `pid_exe_stem_matches`
    /// fails closed for a dead PID, and (separately) for a live PID whose
    /// image is not `fbuild-daemon`. Uses the current process's own PID
    /// (guaranteed alive) with a deliberately wrong expected stem, and a
    /// dead PID (`i32::MAX as u32` — NOT `u32::MAX`, which is `-1` on unix
    /// and would signal the calling process's own process group).
    #[test]
    fn recycled_or_unrelated_pid_is_never_signalled() {
        let own_pid = std::process::id();
        assert!(pid_is_alive(own_pid));
        assert!(!pid_exe_stem_matches(own_pid, "definitely-not-fbuild-daemon"));
        assert!(!should_signal_legacy_pid(
            pid_is_alive(own_pid),
            pid_exe_stem_matches(own_pid, "definitely-not-fbuild-daemon")
        ));

        let dead_pid = i32::MAX as u32;
        assert!(!pid_is_alive(dead_pid));
        assert!(!pid_exe_stem_matches(dead_pid, DAEMON_EXE_STEM));
        assert!(!should_signal_legacy_pid(
            pid_is_alive(dead_pid),
            pid_exe_stem_matches(dead_pid, DAEMON_EXE_STEM)
        ));
    }
}
