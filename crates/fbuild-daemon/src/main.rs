#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use axum::routing::{get, post};
use axum::Router;
use clap::Parser;
use fbuild_daemon::context::{
    BroadcastHub, DaemonContext, IDLE_TIMEOUT, SELF_EVICTION_TIMEOUT, STALE_LOCK_CHECK_INTERVAL,
};
use fbuild_daemon::handlers::{cache, devices, emulator, health, locks, operations, websockets};
use fbuild_daemon::log_layer::BroadcastLogLayer;
use std::sync::Arc;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

#[derive(Parser)]
#[command(name = "fbuild-daemon", about = "fbuild daemon server")]
struct Args {
    /// Port to listen on (default: 8765 prod, 8865 dev)
    #[arg(short, long)]
    port: Option<u16>,

    /// Run in development mode
    #[arg(long)]
    dev: bool,

    /// Working directory of the client that spawned this daemon
    #[arg(long, default_value = "unknown")]
    spawner_cwd: String,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    if args.dev {
        unsafe { std::env::set_var("FBUILD_DEV_MODE", "1") };
    }

    // Install the process-wide containment group as early as possible so
    // every subprocess the daemon spawns (compilers, linkers, esptool,
    // avrdude, qemu, simavr, node, npm, …) is born inside a Windows Job
    // Object (JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE) or a Unix process
    // group with PR_SET_PDEATHSIG. When the daemon dies for any reason
    // — SIGKILL, power loss, crash, console window close — the OS
    // reaps every descendant in the group. See FastLED/fbuild#32.
    if let Err(e) = fbuild_core::containment::init_global_containment("FBUILD-DAEMON") {
        eprintln!("warning: failed to install process containment: {}", e);
    }

    let port = args.port.unwrap_or_else(fbuild_paths::get_daemon_port);

    // Build the broadcast hub before installing the tracing subscriber
    // so the `/ws/logs` bridge layer (issue #66 follow-up) can be
    // registered with the very first tracing event. Any later event —
    // including native ESP32 `write-flash` progress — lands on the same
    // channel that `/ws/logs` subscribers read.
    let broadcast_hub = BroadcastHub::new();
    let log_tx = broadcast_hub.log_tx.clone();

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .with(BroadcastLogLayer::new(log_tx))
        .init();

    tracing::info!("fbuild daemon starting on port {}", port);

    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
    let context = Arc::new(DaemonContext::with_hub(
        port,
        shutdown_tx,
        args.spawner_cwd,
        broadcast_hub,
    ));

    let app = Router::new()
        .route("/", get(health::root))
        .route("/health", get(health::health_check))
        .route("/api/daemon/info", get(health::daemon_info))
        .route("/api/daemon/shutdown", post(health::shutdown))
        .route("/api/build", post(operations::build))
        .route("/api/deploy", post(operations::deploy))
        .route("/api/monitor", post(operations::monitor))
        .route("/api/devices/list", post(devices::list_devices))
        .route("/api/devices/:port/status", get(devices::device_status))
        .route("/api/devices/:port/lease", post(devices::device_lease))
        .route("/api/devices/:port/release", post(devices::device_release))
        .route("/api/devices/:port/preempt", post(devices::device_preempt))
        .route("/api/locks/status", get(locks::lock_status))
        .route("/api/locks/clear", post(locks::clear_locks))
        .route("/api/cache/stats", get(cache::cache_stats))
        .route("/api/cache/gc", post(cache::run_gc))
        .route("/api/install-deps", post(operations::install_deps))
        .route("/api/reset", post(operations::reset))
        .route("/api/test-emu", post(emulator::test_emu))
        .route(
            "/api/emulator/avr8js/:session_id",
            get(emulator::avr8js_session_json),
        )
        .route(
            "/api/emulator/avr8js/:session_id/firmware.hex",
            get(emulator::avr8js_firmware_hex),
        )
        .route("/emulator/avr8js/app.js", get(emulator::avr8js_app_js))
        .route("/emulator/avr8js/:session_id", get(emulator::avr8js_page))
        .route("/ws/serial-monitor", get(websockets::ws_serial_monitor))
        .route("/ws/status", get(websockets::ws_status))
        .route("/ws/logs", get(websockets::ws_logs))
        .route(
            "/ws/monitor/:session_id",
            get(websockets::ws_monitor_session),
        )
        .with_state(context.clone())
        .layer(
            tower_http::cors::CorsLayer::new()
                .allow_origin([
                    "http://localhost".parse().unwrap(),
                    "http://127.0.0.1".parse().unwrap(),
                ])
                .allow_methods(tower_http::cors::Any)
                .allow_headers(tower_http::cors::Any),
        );

    let addr = format!("0.0.0.0:{}", port);
    // Before binding, check whether a stale PID file points at a dead
    // process. If the previous daemon was killed uncleanly, its PID file
    // still exists and any stale TCP state may linger for the protocol
    // timeout. Cleaning up the PID file lets `fbuild daemon list` reflect
    // reality, and the bind retry below handles the kernel-state case.
    // See ISSUES.md "Issue B5a".
    if let Some(stale_pid) = read_stale_daemon_pid() {
        tracing::warn!(
            "found stale daemon PID file pointing at dead PID {}; cleaning up",
            stale_pid
        );
        let _ = std::fs::remove_file(fbuild_paths::get_daemon_pid_file());
    }

    let listener = bind_listener_with_retry(&addr);

    tracing::info!("listening on {}", addr);

    // Write PID file and port file
    let pid_file = fbuild_paths::get_daemon_pid_file();
    let port_file = fbuild_paths::get_daemon_port_file();
    if let Some(parent) = pid_file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(&pid_file, std::process::id().to_string()) {
        tracing::warn!("failed to write PID file: {}", e);
    }
    if let Err(e) = std::fs::write(&port_file, port.to_string()) {
        tracing::warn!("failed to write port file: {}", e);
    }

    // On Windows, `tokio::signal::ctrl_c()` catches CTRL_C_EVENT and
    // CTRL_BREAK_EVENT but NOT CTRL_CLOSE_EVENT (terminal window closed),
    // CTRL_LOGOFF_EVENT, or CTRL_SHUTDOWN_EVENT. Without a console ctrl
    // handler, those events terminate the daemon immediately, skipping
    // graceful shutdown and leaving stale PID/port files. Register a
    // handler that funnels them into the same `shutdown_tx` the HTTP
    // endpoint and Ctrl+C paths use. See FastLED/fbuild#18 ("B5a
    // hardening leftovers").
    #[cfg(windows)]
    windows_console::register_ctrl_handler(context.shutdown_tx.clone());

    // Spawn background maintenance task (self-eviction, idle timeout, stale lock cleanup)
    {
        let ctx = context.clone();
        tokio::spawn(async move {
            let mut daemon_empty_since: Option<std::time::Instant> = None;
            let mut last_lock_cleanup = std::time::Instant::now();
            // Periodic reminder of why the daemon is staying alive, so
            // users can see the blocker rather than thinking self-eviction
            // is broken. See FastLED/fbuild#51.
            let busy_report_interval = std::time::Duration::from_secs(60);
            let mut last_busy_report: Option<std::time::Instant> = None;
            let mut last_busy_reason: Option<String> = None;

            loop {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;

                // --- Self-eviction: 0 ops + 0 serial sessions for SELF_EVICTION_TIMEOUT ---
                match ctx.busy_reason() {
                    None => {
                        if last_busy_reason.is_some() {
                            last_busy_reason = None;
                            last_busy_report = None;
                        }
                        if daemon_empty_since.is_none() {
                            daemon_empty_since = Some(std::time::Instant::now());
                            tracing::info!(
                                "Daemon is idle; self-eviction in {:.0}s unless new work arrives",
                                SELF_EVICTION_TIMEOUT.as_secs_f64()
                            );
                        } else if let Some(since) = daemon_empty_since {
                            if since.elapsed() >= SELF_EVICTION_TIMEOUT {
                                tracing::info!(
                                    "Self-eviction triggered: daemon empty for {:.1}s, shutting down",
                                    since.elapsed().as_secs_f64()
                                );
                                let _ = ctx.shutdown_tx.send(true);
                                return;
                            }
                        }
                    }
                    Some(reason) => {
                        if daemon_empty_since.is_some() {
                            tracing::debug!("Daemon is no longer empty, resetting eviction timer");
                            daemon_empty_since = None;
                        }
                        // Announce the blocker on transition and then
                        // periodically repeat the reminder so a stuck
                        // session is visible in logs without spamming every
                        // tick.
                        let reason_changed = last_busy_reason.as_deref() != Some(reason.as_str());
                        let due_for_reminder = last_busy_report
                            .map(|t| t.elapsed() >= busy_report_interval)
                            .unwrap_or(true);
                        if reason_changed || due_for_reminder {
                            tracing::info!("Daemon staying alive: {}", reason);
                            last_busy_report = Some(std::time::Instant::now());
                            last_busy_reason = Some(reason);
                        }
                    }
                }

                // --- Idle timeout (12h fallback) ---
                if ctx.idle_duration() >= IDLE_TIMEOUT {
                    tracing::info!(
                        "Idle timeout reached ({:.0}s), shutting down",
                        ctx.idle_duration().as_secs_f64()
                    );
                    let _ = ctx.shutdown_tx.send(true);
                    return;
                }

                // --- Periodic stale lock cleanup ---
                if last_lock_cleanup.elapsed() >= STALE_LOCK_CHECK_INTERVAL {
                    last_lock_cleanup = std::time::Instant::now();
                    let mut cleared = 0usize;
                    let keys: Vec<_> = ctx.project_locks.iter().map(|e| e.key().clone()).collect();
                    for key in keys {
                        if let Some(entry) = ctx.project_locks.get(&key) {
                            if entry.value().try_lock().is_ok() {
                                drop(entry);
                                ctx.project_locks.remove(&key);
                                cleared += 1;
                            }
                        }
                    }
                    if cleared > 0 {
                        tracing::info!(
                            "Periodic cleanup: cleared {} stale project lock(s)",
                            cleared
                        );
                    }
                }
            }
        });
    }

    // Run startup reconciliation in a background task — cleans up partial
    // installs and orphan files left by crashed previous instances.
    {
        tokio::spawn(async {
            match fbuild_packages::DiskCache::open() {
                Ok(dc) => match dc.reconcile() {
                    Ok(report) => {
                        if report.orphan_files_removed > 0 || report.orphan_rows_cleaned > 0 {
                            tracing::info!(
                                "startup reconciliation: cleaned {} orphan files, {} orphan rows",
                                report.orphan_files_removed,
                                report.orphan_rows_cleaned,
                            );
                        }
                    }
                    Err(e) => tracing::warn!("startup reconciliation failed: {}", e),
                },
                Err(e) => tracing::debug!("could not open cache for reconciliation: {}", e),
            }
        });
    }

    // Spawn background GC loop — runs every 5 minutes
    {
        let gc_mutex = context.gc_mutex.clone();
        tokio::spawn(async move {
            // Wait 60s after startup before first GC to avoid slowing boot
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;

            let gc_interval = std::time::Duration::from_secs(300);
            loop {
                {
                    // Serialize with manual /api/cache/gc endpoint.
                    // Scope the guard so it's dropped before the sleep.
                    let _guard = gc_mutex.lock().await;
                    match fbuild_packages::DiskCache::open() {
                        Ok(dc) => match dc.run_gc() {
                            Ok(report) => {
                                if report.total_bytes_freed() > 0 {
                                    tracing::info!(
                                        "background GC: freed {} installed ({} entries) + {} archives ({} entries)",
                                        format_bytes_compact(report.installed_bytes_freed),
                                        report.installed_evicted,
                                        format_bytes_compact(report.archive_bytes_freed),
                                        report.archives_evicted,
                                    );
                                }
                            }
                            Err(e) => {
                                tracing::warn!("background GC failed: {}", e);
                            }
                        },
                        Err(e) => {
                            tracing::debug!("background GC: could not open cache: {}", e);
                        }
                    }
                }
                tokio::time::sleep(gc_interval).await;
            }
        });
    }

    // Clone refs so we can check operation state on Ctrl+C
    let shutdown_tx_signal = context.shutdown_tx.clone();
    let op_in_progress = context.operation_in_progress.clone();

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            // Wait for either the HTTP shutdown endpoint or Ctrl+C / SIGTERM
            tokio::select! {
                _ = async { let _ = shutdown_rx.changed().await; } => {
                    tracing::info!("shutdown requested via HTTP");
                }
                _ = async {
                    // On Ctrl+C, refuse shutdown if an operation is in progress
                    // (matches Python daemon behavior)
                    loop {
                        tokio::signal::ctrl_c().await.ok();
                        if op_in_progress.load(std::sync::atomic::Ordering::Relaxed) {
                            tracing::warn!("SIGINT received during operation — ignoring. Use kill -9 to force.");
                            eprintln!("⚠ Cannot shutdown while operation is active. Use kill -9 to force.");
                        } else {
                            tracing::info!("shutdown requested via Ctrl+C");
                            let _ = shutdown_tx_signal.send(true);
                            break;
                        }
                    }
                } => {}
            }
            tracing::info!("graceful shutdown initiated");
        })
        .await
        .unwrap_or_else(|e| {
            tracing::error!("server error: {}", e);
        });

    // Clean up PID and port files
    let _ = std::fs::remove_file(&pid_file);
    let _ = std::fs::remove_file(&port_file);

    tracing::info!("daemon exiting");
    std::process::exit(0);
}

/// Read the daemon PID file. Returns `Some(pid)` only if the file exists,
/// the contents parse as a u32, AND the referenced process is no longer
/// alive (i.e. it's safe to clean up). Returns `None` if the file is
/// missing, unreadable, malformed, or points at a still-running process.
///
/// Used at startup to clean up after a crashed previous instance so that
/// `fbuild daemon list` reflects reality and the bind-retry loop has a
/// chance to claim the port. See ISSUES.md "Issue B5a".
fn read_stale_daemon_pid() -> Option<u32> {
    let path = fbuild_paths::get_daemon_pid_file();
    let raw = std::fs::read_to_string(&path).ok()?;
    let pid: u32 = raw.trim().parse().ok()?;
    if is_pid_alive(pid) {
        None
    } else {
        Some(pid)
    }
}

/// Cross-platform "is this PID still running?" check. Avoids dragging in
/// the `sysinfo` crate for a 10-line operation.
fn is_pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: kill(pid, 0) is a probe — it sends no signal but
        // returns 0 if the PID exists and we have permission, or -1
        // with errno=ESRCH if the PID does not exist.
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(windows)]
    {
        // OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION) succeeds for any
        // running process; ERROR_INVALID_PARAMETER (87) means the PID is
        // gone. We use the limited variant so the probe works for
        // processes owned by other users / elevated daemons.
        const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
        type Handle = *mut std::ffi::c_void;
        #[link(name = "kernel32")]
        extern "system" {
            fn OpenProcess(desired_access: u32, inherit_handle: i32, process_id: u32) -> Handle;
            fn CloseHandle(handle: Handle) -> i32;
        }
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                false
            } else {
                CloseHandle(h);
                true
            }
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        true
    }
}

/// Bind the daemon's TCP listener with platform-appropriate hardening:
///
/// * **Windows** — sets `SO_EXCLUSIVEADDRUSE` so no other local process
///   can hijack the port via `SO_REUSEADDR`. The earlier B5 mitigation
///   used `SO_REUSEADDR` instead, which on Windows permits multi-binder
///   takeover (Linux/BSD `SO_REUSEADDR` only allows `TIME_WAIT` recovery,
///   so it's safe there but not on Windows). See ISSUES.md "Issue B5a".
/// * **Unix** — sets `SO_REUSEADDR`, which only permits `TIME_WAIT`
///   recovery on this OS family.
/// * **All platforms** — sets `SO_LINGER = 0` on the listening socket so
///   that accepted client sockets inherit a zero linger and force an
///   immediate `RST` on close instead of going through the
///   `FIN / CLOSE_WAIT / TIME_WAIT` dance. After a hard-kill of the
///   daemon, this prevents the kernel from leaking dangling `CLOSE_WAIT`
///   state on client sockets that outlives the daemon itself and would
///   otherwise block a fresh instance from re-binding the port. SO_LINGER
///   is inherited from the listener by `accept(2)` on Linux, macOS, and
///   Windows (AFD.sys), so setting it once on the listener covers every
///   subsequently accepted connection without needing to hook axum 0.7's
///   internal accept loop. See FastLED/fbuild#32.
///
/// Bind is retried up to 3 times with 500 ms backoff to handle the brief
/// window where a hard-killed previous instance still has kernel TCP
/// state. Permanent failures (port owned by a live process, permission
/// denied) bubble up after the retries are exhausted.
fn bind_listener_with_retry(addr: &str) -> tokio::net::TcpListener {
    use socket2::{Domain, Protocol, Socket, Type};
    let std_addr: std::net::SocketAddr = addr.parse().unwrap_or_else(|e| {
        eprintln!("invalid bind address {}: {}", addr, e);
        std::process::exit(1);
    });

    let mut last_err: Option<std::io::Error> = None;
    for attempt in 0..3u32 {
        let sock = match Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP)) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("failed to create socket: {}", e);
                std::process::exit(1);
            }
        };

        // Apply platform-specific address-reuse policy.
        #[cfg(windows)]
        {
            if let Err(e) = set_exclusive_address_windows(&sock) {
                tracing::warn!("failed to set SO_EXCLUSIVEADDRUSE: {}", e);
            }
        }
        #[cfg(not(windows))]
        {
            if let Err(e) = sock.set_reuse_address(true) {
                tracing::warn!("failed to set SO_REUSEADDR: {}", e);
            }
        }

        // Force RST on close for accepted client sockets — inherited via
        // `accept(2)` on Linux/macOS/Windows. See doc comment above and
        // FastLED/fbuild#32.
        if let Err(e) = sock.set_linger(Some(std::time::Duration::ZERO)) {
            tracing::warn!("failed to set SO_LINGER=0 on listener: {}", e);
        }

        if let Err(e) = sock.set_nonblocking(true) {
            eprintln!("failed to set non-blocking: {}", e);
            std::process::exit(1);
        }

        match sock.bind(&std_addr.into()) {
            Ok(()) => match sock.listen(128) {
                Ok(()) => {
                    let std_listener: std::net::TcpListener = sock.into();
                    return tokio::net::TcpListener::from_std(std_listener).unwrap_or_else(|e| {
                        eprintln!("failed to convert listener to tokio: {}", e);
                        std::process::exit(1);
                    });
                }
                Err(e) => {
                    eprintln!("failed to listen on {}: {}", addr, e);
                    std::process::exit(1);
                }
            },
            Err(e) => {
                tracing::warn!(
                    attempt,
                    "bind to {} failed ({}); retrying in 500ms",
                    addr,
                    e
                );
                last_err = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        }
    }

    eprintln!(
        "failed to bind to {} after 3 attempts: {}",
        addr,
        last_err
            .map(|e| e.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    );
    std::process::exit(1);
}

/// Set `SO_EXCLUSIVEADDRUSE` on a Windows socket using a manual FFI call,
/// since socket2 0.6 does not yet expose this option. The constant value
/// is `~SO_REUSEADDR = -5` (i.e. the bitwise complement of `SO_REUSEADDR`).
/// `SOL_SOCKET` on Winsock is `0xFFFF`, NOT `1` like on Linux.
#[cfg(windows)]
fn set_exclusive_address_windows(sock: &socket2::Socket) -> std::io::Result<()> {
    use std::os::windows::io::AsRawSocket;

    const SOL_SOCKET: i32 = 0xFFFF;
    const SO_EXCLUSIVEADDRUSE: i32 = !0x0004; // = -5

    type SocketHandle = usize;
    #[link(name = "ws2_32")]
    extern "system" {
        fn setsockopt(
            s: SocketHandle,
            level: i32,
            optname: i32,
            optval: *const u8,
            optlen: i32,
        ) -> i32;
    }

    let raw: SocketHandle = sock.as_raw_socket() as SocketHandle;
    let on: i32 = 1;
    let ret = unsafe {
        setsockopt(
            raw,
            SOL_SOCKET,
            SO_EXCLUSIVEADDRUSE,
            &on as *const i32 as *const u8,
            std::mem::size_of::<i32>() as i32,
        )
    };
    if ret == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Windows-only console ctrl handler registration.
///
/// `tokio::signal::ctrl_c()` covers CTRL_C_EVENT / CTRL_BREAK_EVENT on
/// Windows but the console subsystem also fires CTRL_CLOSE_EVENT (window
/// X button), CTRL_LOGOFF_EVENT, and CTRL_SHUTDOWN_EVENT — each of which
/// terminates the process unless an explicit handler is registered via
/// `SetConsoleCtrlHandler`. Without the hook, the daemon dies without
/// running its graceful-shutdown path, leaving stale PID/port files and
/// potentially orphaned child processes. See FastLED/fbuild#18 ("B5a
/// hardening leftovers").
#[cfg(windows)]
mod windows_console {
    use std::sync::OnceLock;
    use tokio::sync::watch;

    /// Globally accessible shutdown sender — the console ctrl handler
    /// has a fixed C-ABI signature with no user-data pointer, so the only
    /// way to reach the daemon's shutdown channel from inside it is
    /// through process-wide state.
    static SHUTDOWN_TX: OnceLock<watch::Sender<bool>> = OnceLock::new();

    /// Windows console control events: `CTRL_CLOSE_EVENT = 2`,
    /// `CTRL_LOGOFF_EVENT = 5`, `CTRL_SHUTDOWN_EVENT = 6`. `CTRL_C_EVENT`
    /// and `CTRL_BREAK_EVENT` are already covered by `tokio::signal::ctrl_c`
    /// so we deliberately fall through (return 0 / FALSE) to let the
    /// default handler chain propagate them to tokio's signal driver.
    unsafe extern "system" fn console_ctrl_handler(ctrl_type: u32) -> i32 {
        const CTRL_CLOSE_EVENT: u32 = 2;
        const CTRL_LOGOFF_EVENT: u32 = 5;
        const CTRL_SHUTDOWN_EVENT: u32 = 6;

        match ctrl_type {
            CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT => {
                if let Some(tx) = SHUTDOWN_TX.get() {
                    let _ = tx.send(true);
                    // Windows gives a CTRL_CLOSE handler ~5s and a
                    // CTRL_SHUTDOWN handler ~20s before it force-kills
                    // the process. Block here so the main graceful-shutdown
                    // path has a chance to run to completion; if it
                    // finishes sooner, the process exits normally from
                    // `main` and this sleep is cut short by that exit.
                    std::thread::sleep(std::time::Duration::from_millis(3500));
                }
                1 // TRUE — handled
            }
            _ => 0, // FALSE — let the default handler take it
        }
    }

    pub fn register_ctrl_handler(shutdown_tx: watch::Sender<bool>) {
        // Idempotent on repeated calls; `OnceLock::set` returns Err if
        // already initialised — we ignore it.
        let _ = SHUTDOWN_TX.set(shutdown_tx);

        #[link(name = "kernel32")]
        extern "system" {
            fn SetConsoleCtrlHandler(
                handler_routine: Option<unsafe extern "system" fn(u32) -> i32>,
                add: i32,
            ) -> i32;
        }

        let ret = unsafe { SetConsoleCtrlHandler(Some(console_ctrl_handler), 1) };
        if ret == 0 {
            tracing::warn!(
                "SetConsoleCtrlHandler failed (err={}); \
                 CTRL_CLOSE/LOGOFF/SHUTDOWN events will bypass graceful shutdown",
                std::io::Error::last_os_error()
            );
        }
    }
}

/// Compact byte formatter for log messages.
fn format_bytes_compact(bytes: u64) -> String {
    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;
    const KIB: u64 = 1024;
    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{} B", bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_pid_alive_returns_true_for_self() {
        // Our own PID must always be reported alive — this confirms the
        // OpenProcess/kill probe is wired up correctly on each platform.
        assert!(
            is_pid_alive(std::process::id()),
            "is_pid_alive(self) must return true"
        );
    }

    #[test]
    fn is_pid_alive_returns_false_for_obviously_dead_pid() {
        // PID 0 is reserved (Windows: System Idle Process; Unix: kernel
        // task scheduler) and the OpenProcess / kill(0,0) probes both
        // refuse to operate on it. Use a very large PID instead, well
        // outside any plausible PID range.
        let likely_dead: u32 = 4_000_000_000;
        assert!(
            !is_pid_alive(likely_dead),
            "is_pid_alive({}) must return false",
            likely_dead
        );
    }
}
