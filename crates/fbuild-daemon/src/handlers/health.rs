//! Health check, daemon info, root, and shutdown handlers.

use crate::context::DaemonContext;
use crate::models::{
    DaemonInfoResponse, HealthResponse, HeapDumpResponse, ImageHashResponse, RootResponse,
    ShutdownParams, ShutdownResponse,
};
use axum::Json;
use axum::extract::{ConnectInfo, Query, State};
use axum::http::{HeaderMap, StatusCode};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// GET /
pub async fn root() -> Json<RootResponse> {
    Json(RootResponse {
        message: "fbuild Daemon API".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        health: "/health".to_string(),
    })
}

/// GET /health
pub async fn health_check(State(ctx): State<Arc<DaemonContext>>) -> Json<HealthResponse> {
    ctx.touch_activity();
    Json(HealthResponse {
        status: "healthy".to_string(),
        uptime_seconds: ctx.started_at.elapsed().as_secs_f64(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        pid: std::process::id(),
        source_mtime: ctx.source_mtime,
        source_exe: ctx.source_exe.clone(),
        launched_by_broker: ctx.launched_by_broker,
    })
}

/// Hash the running image on demand. On Linux `/proc/self/exe` names the
/// loaded inode even if the original path was replaced after startup.
pub async fn image_hash(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    State(ctx): State<Arc<DaemonContext>>,
) -> Result<Json<ImageHashResponse>, StatusCode> {
    if !peer.ip().is_loopback() {
        return Err(StatusCode::FORBIDDEN);
    }
    ctx.touch_activity();
    let hash = tokio::task::spawn_blocking(move || {
        ctx.source_hash
            .get_or_init(|| {
                #[cfg(target_os = "linux")]
                let path = std::path::Path::new("/proc/self/exe");
                #[cfg(not(target_os = "linux"))]
                let path = {
                    let path = std::path::Path::new(&ctx.source_exe);
                    // A replaced pathname no longer identifies the running image.
                    // If it changed after startup, retain the mtime restart decision.
                    let current_mtime = path
                        .metadata()
                        .and_then(|meta| meta.modified())
                        .ok()
                        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|duration| duration.as_secs_f64());
                    if current_mtime != Some(ctx.source_mtime) {
                        return None;
                    }
                    path
                };
                fbuild_paths::executable_hash::blake3_file(path)
                    .map(|digest| digest.to_hex().to_string())
                    .map_err(|error| tracing::warn!("cannot hash daemon image: {error}"))
                    .ok()
            })
            .clone()
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(ImageHashResponse {
        pid: std::process::id(),
        blake3: hash,
    }))
}

#[cfg(test)]
mod image_hash_tests {
    use super::*;

    #[tokio::test]
    async fn hashes_loaded_image_only_for_loopback_callers() {
        let (shutdown, _receiver) = tokio::sync::watch::channel(false);
        let ctx = Arc::new(DaemonContext::new(8765, shutdown, "test".into()));
        assert!(ctx.source_hash.get().is_none());

        let remote = "192.0.2.1:1234".parse().unwrap();
        let denied = image_hash(ConnectInfo(remote), State(Arc::clone(&ctx))).await;
        assert!(matches!(denied, Err(StatusCode::FORBIDDEN)));
        assert!(ctx.source_hash.get().is_none());

        let local = "127.0.0.1:1234".parse().unwrap();
        let response = image_hash(ConnectInfo(local), State(Arc::clone(&ctx)))
            .await
            .unwrap()
            .0;
        #[cfg(target_os = "linux")]
        let path = std::path::Path::new("/proc/self/exe");
        #[cfg(not(target_os = "linux"))]
        let path = std::path::Path::new(&ctx.source_exe);
        let expected = fbuild_paths::executable_hash::blake3_file(path)
            .unwrap()
            .to_hex()
            .to_string();
        assert_eq!(response.pid, std::process::id());
        assert_eq!(response.blake3, expected);
        assert!(ctx.source_hash.get().is_some());
    }
}

/// GET /api/daemon/info
pub async fn daemon_info(State(ctx): State<Arc<DaemonContext>>) -> Json<DaemonInfoResponse> {
    ctx.touch_activity();
    let daemon_state = *ctx.daemon_state.read().unwrap_or_else(|e| e.into_inner());
    let current_operation = ctx
        .current_operation
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let cache_identity = fbuild_paths::running_process::DaemonCacheIdentity::discover();
    Json(DaemonInfoResponse {
        status: "running".to_string(),
        uptime_seconds: ctx.started_at.elapsed().as_secs_f64(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        pid: std::process::id(),
        port: ctx.port,
        started_at: ctx.started_at_unix,
        dev_mode: fbuild_paths::is_dev_mode(),
        host: "127.0.0.1".to_string(),
        operation_in_progress: ctx.operation_in_progress.load(Ordering::Relaxed),
        daemon_state,
        current_operation,
        dependency_install: ctx.dependency_install_snapshot(),
        client_count: ctx.serial_manager.get_port_sessions().len(),
        cache_dir: cache_identity.cache_root.to_string_lossy().to_string(),
        cache_identity: cache_identity.label_value(),
        cache_schema_version: fbuild_paths::running_process::CACHE_SCHEMA_VERSION,
        daemon_dir: fbuild_paths::get_daemon_dir().to_string_lossy().to_string(),
        source_mtime: ctx.source_mtime,
        spawner_cwd: ctx.spawner_cwd.clone(),
        mcp_url: format!("http://127.0.0.1:{}/mcp", ctx.port),
        watch_set_cache: Some(ctx.watch_set_cache.stats()),
    })
}

/// POST /api/daemon/heap-dump
///
/// Write a pprof heap snapshot of this daemon and return where it landed.
///
/// Deliberately reachable on a daemon that is already misbehaving, which is
/// the case FastLED/fbuild#1360 ran into: the process had grown to ~3.9 GB and
/// restarting it to turn on a profiler would have destroyed the very leak
/// under investigation.
///
/// Starts a session on demand when none is running, so an operator who did
/// not set `FBUILD_HEAP_PROFILE` at startup still gets something. That
/// snapshot only covers allocations made *after* this call — the response
/// says so rather than letting a thin profile read as "nothing is leaking".
pub async fn heap_dump(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> (StatusCode, Json<HeapDumpResponse>) {
    // Loopback only. The daemon binds 0.0.0.0 (see `main.rs`), and this
    // endpoint is not a read: it can switch process-wide profiling on and make
    // the daemon serialize its whole heap on demand. Reachable from off-box
    // that is both a denial-of-service lever and a way to extract allocation
    // shapes from someone else's machine. Nothing about a profiling dump needs
    // to cross a network boundary, so the check is a flat refusal rather than
    // a rate limit.
    if !peer.ip().is_loopback() {
        tracing::warn!(peer = %peer, "heap-dump refused: non-loopback caller");
        return (
            StatusCode::FORBIDDEN,
            Json(HeapDumpResponse {
                path: None,
                live_samples: 0,
                profiling_was_already_running: crate::heap_profile::is_enabled(),
                message: "heap-dump is loopback-only".to_string(),
            }),
        );
    }

    let was_running = crate::heap_profile::is_enabled();
    if !was_running {
        crate::heap_profile::start(DEFAULT_ON_DEMAND_SAMPLE_RATE);
    }

    match crate::heap_profile::dump(None).await {
        Ok(path) => (
            StatusCode::OK,
            Json(HeapDumpResponse {
                path: Some(path.display_slash()),
                live_samples: crate::heap_profile::live_sample_count(),
                profiling_was_already_running: was_running,
                message: if was_running {
                    "heap snapshot written".to_string()
                } else {
                    "heap snapshot written, but profiling only started with this                      request — it covers allocations from now on, not the ones                      already held. Set FBUILD_HEAP_PROFILE=1 before starting the                      daemon to capture from process start."
                        .to_string()
                },
            }),
        ),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(HeapDumpResponse {
                path: None,
                live_samples: 0,
                profiling_was_already_running: was_running,
                message: format!("heap dump failed: {error}"),
            }),
        ),
    }
}

/// Sample rate used when a dump is requested on a daemon that was not started
/// with profiling on. 64 KiB is finer than the 512 KiB default: by this point
/// someone is actively chasing something, and the extra resolution is worth
/// more than the overhead.
const DEFAULT_ON_DEMAND_SAMPLE_RATE: usize = 64 * 1024;

/// POST /api/daemon/shutdown
pub async fn shutdown(
    State(ctx): State<Arc<DaemonContext>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    query: Query<ShutdownParams>,
) -> (StatusCode, Json<ShutdownResponse>) {
    let force = query.force.unwrap_or(false);
    let caller = ShutdownCaller::from_headers(peer, &headers);

    if ctx.try_begin_shutdown(force).is_none() {
        tracing::warn!(
            peer = %caller.peer,
            client_pid = caller.pid.as_deref().unwrap_or("unknown"),
            client_cwd = caller.cwd.as_deref().unwrap_or("unknown"),
            client_exe = caller.exe.as_deref().unwrap_or("unknown"),
            client_argv = caller.argv.as_deref().unwrap_or("unknown"),
            current_operation = current_operation_for_log(&ctx).as_deref().unwrap_or("unknown"),
            "shutdown refused: operation in progress"
        );
        return (
            StatusCode::CONFLICT,
            Json(ShutdownResponse {
                message: "operation in progress; use ?force=true to force shutdown".to_string(),
            }),
        );
    }

    let _ = ctx.shutdown_tx.send(true);
    tracing::info!(
        peer = %caller.peer,
        client_pid = caller.pid.as_deref().unwrap_or("unknown"),
        client_cwd = caller.cwd.as_deref().unwrap_or("unknown"),
        client_exe = caller.exe.as_deref().unwrap_or("unknown"),
        client_argv = caller.argv.as_deref().unwrap_or("unknown"),
        force,
        current_operation = current_operation_for_log(&ctx).as_deref().unwrap_or("none"),
        "shutdown requested"
    );
    (
        StatusCode::OK,
        Json(ShutdownResponse {
            message: "shutting down".to_string(),
        }),
    )
}

#[derive(Debug)]
struct ShutdownCaller {
    peer: SocketAddr,
    pid: Option<String>,
    cwd: Option<String>,
    exe: Option<String>,
    argv: Option<String>,
}

impl ShutdownCaller {
    fn from_headers(peer: SocketAddr, headers: &HeaderMap) -> Self {
        Self {
            peer,
            pid: shutdown_header(headers, "x-fbuild-client-pid"),
            cwd: shutdown_header(headers, "x-fbuild-client-cwd"),
            exe: shutdown_header(headers, "x-fbuild-client-exe"),
            argv: shutdown_header(headers, "x-fbuild-client-argv"),
        }
    }
}

fn shutdown_header(headers: &HeaderMap, name: &'static str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
}

fn current_operation_for_log(ctx: &DaemonContext) -> Option<String> {
    ctx.current_operation
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn test_context() -> Arc<DaemonContext> {
        let (shutdown_tx, _shutdown_rx) = tokio::sync::watch::channel(false);
        Arc::new(DaemonContext::new(8765, shutdown_tx, "test".to_string()))
    }

    #[test]
    fn shutdown_caller_extracts_client_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("x-fbuild-client-pid", HeaderValue::from_static("1234"));
        headers.insert(
            "x-fbuild-client-cwd",
            HeaderValue::from_static("C:/work/fastled"),
        );
        headers.insert(
            "x-fbuild-client-exe",
            HeaderValue::from_static("C:/tools/fbuild.exe"),
        );
        headers.insert(
            "x-fbuild-client-argv",
            HeaderValue::from_static("fbuild build"),
        );

        let caller = ShutdownCaller::from_headers("127.0.0.1:5555".parse().unwrap(), &headers);

        assert_eq!(caller.pid.as_deref(), Some("1234"));
        assert_eq!(caller.cwd.as_deref(), Some("C:/work/fastled"));
        assert_eq!(caller.exe.as_deref(), Some("C:/tools/fbuild.exe"));
        assert_eq!(caller.argv.as_deref(), Some("fbuild build"));
    }

    #[tokio::test]
    async fn shutdown_refuses_non_force_when_operation_in_progress() {
        let ctx = test_context();
        ctx.operation_in_progress.store(true, Ordering::Relaxed);
        *ctx.current_operation.write().unwrap() = Some("Building C:/work/fastled".to_string());

        let (status, body) = shutdown(
            State(ctx.clone()),
            ConnectInfo("127.0.0.1:5555".parse().unwrap()),
            HeaderMap::new(),
            Query(ShutdownParams { force: None }),
        )
        .await;

        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(
            body.message,
            "operation in progress; use ?force=true to force shutdown"
        );
        assert!(!ctx.is_shutting_down.load(Ordering::Relaxed));
    }
}
