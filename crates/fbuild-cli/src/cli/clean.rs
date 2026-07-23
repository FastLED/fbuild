//! Standalone project/framework cleanup command.

use crate::daemon_client::{self, BuildRequest, DaemonClient};
use crate::output;
use fbuild_core::file_lock::{self, FileLockGuard, FileLockMode};
use std::future::Future;
use std::io;
use std::path::Path;
use std::time::Duration;

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

async fn reset_compiler_cache() -> fbuild_core::Result<()> {
    let compiler_cache = fbuild_paths::get_fbuild_root().join("zccache");
    let reset_gate = acquire_cache_lock(
        &fbuild_paths::get_daemon_cache_reset_gate_file(),
        FileLockMode::Exclusive,
        "fbuild-daemon startup gate",
    )
    .await?;

    let client = DaemonClient::new();
    validate_running_daemon_identity(&client).await?;

    let stop_result = run_daemon(DaemonAction::Stop).await;
    if let Err(stop_error) = stop_result {
        let daemon_stayed_healthy = daemon_stays_healthy(&client).await;
        accept_stop_outcome(Err(stop_error), daemon_stayed_healthy)?;
    }

    let active_lock = match acquire_cache_lock(
        &fbuild_paths::get_daemon_cache_lifecycle_lock_file(),
        FileLockMode::Exclusive,
        "fbuild-daemon cache owner to exit",
    )
    .await
    {
        Ok(lock) => lock,
        Err(lock_error) => {
            drop(reset_gate);
            let restore_result = daemon_client::ensure_daemon_running().await;
            return combine_reset_results(Err(lock_error), restore_result);
        }
    };

    let cleanup_result = remove_compiler_cache_at(&compiler_cache).await;
    drop(active_lock);
    drop(reset_gate);
    finish_cache_reset(cleanup_result, || run_daemon(DaemonAction::Restart)).await?;

    output::result(format!(
        "global compiler cache cleared: {}",
        compiler_cache.display()
    ));
    Ok(())
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

async fn acquire_cache_lock(
    path: &Path,
    mode: FileLockMode,
    purpose: &str,
) -> fbuild_core::Result<FileLockGuard> {
    const LOCK_TIMEOUT: Duration = Duration::from_secs(30);
    const POLL_INTERVAL: Duration = Duration::from_millis(100);

    file_lock::acquire(path, mode, LOCK_TIMEOUT, POLL_INTERVAL)
        .await
        .map_err(|error| {
            fbuild_core::FbuildError::DaemonError(format!(
                "failed while waiting for {purpose} at {}: {error}",
                path.display()
            ))
        })
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
}
