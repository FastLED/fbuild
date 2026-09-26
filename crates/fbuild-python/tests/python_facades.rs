//! Rust integration tests that embed CPython to exercise the Python-visible
//! `SerialMonitor` / `AsyncSerialMonitor` facades under real GIL and asyncio
//! behavior (FastLED/fbuild#1485 §5.2, "AT-P" tests).
//!
//! These need `libpython` at link time and at runtime (the `pyo3
//! auto-initialize` dev-dependency feature), so every test here is marked
//! `#[ignore = "embeds CPython; run by the python-facade CI job"]` and run
//! with `--ignored` by the dedicated CI job
//! (`.github/workflows/check-ubuntu.yml` job `python-facade-tests`) rather
//! than the default `bash test` sweep.
//!
//! ## Fake daemon
//!
//! `get_daemon_port()`/`get_daemon_url()` (`fbuild-paths`) derive from a
//! single `FBUILD_DAEMON_PORT` env var, so the WebSocket
//! (`/ws/serial-monitor`) and the HTTP reset endpoint (`/api/reset`) must be
//! served on the **same** port. `start_fake_daemon` runs one listener that
//! peeks each new connection's first bytes to demux: `POST` goes to the
//! HTTP reset stub, anything else goes to the WebSocket handshake.
//!
//! ## Scope note
//!
//! AT-P1..AT-P16 are implemented here; the "each" per-method
//! GIL check in the issue's AT-P11 is implemented as one parameterized test
//! per representative blocking method rather than a full cross product.
//!
//! Point `PYO3_PYTHON` at a local Python 3.10+ if auto-detection picks the
//! wrong interpreter (this repo's abi3-py310 floor tracks FastLED/fbuild#1451).

use std::time::Duration;

use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite;

/// Knobs for the in-process fake daemon.
#[derive(Clone, Default)]
struct DaemonKnobs {
    attach_delay: Duration,
    /// Delay before each `write_ack` — long enough that a Python thread
    /// blocked on the call can be observed *not* holding the GIL.
    ack_delay: Duration,
    reset_delay: Duration,
    /// Reply `/api/reset` with this `success` value.
    reset_success: bool,
    /// Emit an unsolicited "echo:boot" data line right after every WS
    /// attach, simulating a device that has just rebooted and started
    /// producing output. Used by AT-P8 (`reset_device(wait_for_output=True)`
    /// needs *some* post-reset output to observe); left off elsewhere so it
    /// doesn't perturb other tests' exact line counts.
    emit_boot_line: bool,
    fail_first_write: bool,
}

async fn handle_ws(stream: tokio::net::TcpStream, knobs: DaemonKnobs) {
    let ws = match tokio_tungstenite::accept_async(stream).await {
        Ok(ws) => ws,
        Err(_) => return,
    };
    let (mut sink, mut source) = ws.split();
    let mut attached = false;
    let mut write_count = 0usize;
    while let Some(Ok(msg)) = source.next().await {
        let tungstenite::Message::Text(text) = msg else {
            continue;
        };
        let Ok(request) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        if !attached {
            attached = true;
            if !knobs.attach_delay.is_zero() {
                tokio::time::sleep(knobs.attach_delay).await;
            }
            let ack = serde_json::json!({
                "type": "attached", "success": true, "message": "ok", "writer_pre_acquired": true
            });
            if sink
                .send(tungstenite::Message::Text(ack.to_string()))
                .await
                .is_err()
            {
                return;
            }
            if knobs.emit_boot_line {
                let line = serde_json::json!({
                    "type": "data", "lines": ["echo:boot"], "current_index": 0
                })
                .to_string();
                let _ = sink.send(tungstenite::Message::Text(line)).await;
            }
            continue;
        }
        match request["type"].as_str() {
            Some("write") => {
                write_count += 1;
                let data = request["data"].as_str().unwrap_or_default().to_string();
                let decoded: Vec<u8> = base64::Engine::decode(
                    &base64::engine::general_purpose::STANDARD,
                    data.as_bytes(),
                )
                .unwrap_or_default();
                if !knobs.ack_delay.is_zero() {
                    tokio::time::sleep(knobs.ack_delay).await;
                }
                if knobs.fail_first_write && write_count == 1 {
                    let ack = serde_json::json!({
                        "type": "write_ack", "success": false,
                        "bytes_written": 0, "message": "injected serial failure"
                    });
                    if sink
                        .send(tungstenite::Message::Text(ack.to_string()))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    continue;
                }
                let ack = serde_json::json!({
                    "type": "write_ack", "success": true, "bytes_written": decoded.len(), "message": null
                });
                if sink
                    .send(tungstenite::Message::Text(ack.to_string()))
                    .await
                    .is_err()
                {
                    return;
                }
                let text = String::from_utf8_lossy(&decoded).trim().to_string();
                // A real device's own serial output that starts with
                // "REMOTE:" is its JSON-RPC reply. The fake daemon has no
                // real device, so it simulates one: any write whose
                // payload parses as JSON (the shape `write_json_rpc`
                // sends) is treated as a request the "device" echoes back
                // verbatim as its `REMOTE:` reply; anything else is a
                // plain line echo, and text that is already `REMOTE:`-
                // prefixed (used directly by a couple of low-level tests)
                // passes through unchanged.
                let echoed = if text.starts_with("REMOTE:") {
                    text
                } else if serde_json::from_str::<serde_json::Value>(&text)
                    .is_ok_and(|v| v.is_object())
                {
                    format!("REMOTE:{text}")
                } else {
                    format!("echo:{text}")
                };
                let line = serde_json::json!({
                    "type": "data", "lines": [echoed], "current_index": 0
                })
                .to_string();
                if sink.send(tungstenite::Message::Text(line)).await.is_err() {
                    return;
                }
            }
            Some("get_in_waiting") => {
                if !knobs.ack_delay.is_zero() {
                    tokio::time::sleep(knobs.ack_delay).await;
                }
                let reply = serde_json::json!({ "type": "in_waiting", "count": 0 });
                if sink
                    .send(tungstenite::Message::Text(reply.to_string()))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Some("clear_buffer") => {}
            Some("detach") => record_daemon_event("detach"),
            _ => {}
        }
    }
    record_daemon_event("close");
}

fn record_daemon_event(event: &str) {
    use std::io::Write;
    let Ok(path) = std::env::var("FBUILD_FACADE_DAEMON_EVENT_FILE") else {
        return;
    };
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{event}");
    }
}

/// Minimal `POST /api/reset` stub: drains the request, replies 200 with
/// `{"success": knobs.reset_success}` as JSON.
async fn handle_http_reset(mut stream: tokio::net::TcpStream, knobs: DaemonKnobs) {
    let mut buf = [0u8; 4096];
    let _ = stream.read(&mut buf).await;
    if !knobs.reset_delay.is_zero() {
        tokio::time::sleep(knobs.reset_delay).await;
    }
    let body = format!("{{\"success\":{}}}", knobs.reset_success);
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(resp.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// One listener demuxing WebSocket upgrades and `POST /api/reset` by
/// peeking each new connection's first bytes (see module docs).
async fn accept_dual(listener: tokio::net::TcpListener, knobs: DaemonKnobs) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let _ = stream.set_nodelay(true);
        let knobs = knobs.clone();
        tokio::spawn(async move {
            let mut peek_buf = [0u8; 4];
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            loop {
                match stream.peek(&mut peek_buf).await {
                    Ok(n) if n >= 4 => break,
                    Ok(_) if tokio::time::Instant::now() < deadline => {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                    _ => return,
                }
            }
            if &peek_buf == b"POST" {
                handle_http_reset(stream, knobs).await;
            } else {
                handle_ws(stream, knobs).await;
            }
        });
    }
}

/// The test binary re-executes this ignored test as a child process. Keeping
/// the fake daemon outside the embedded interpreter's process is part of
/// #1485 §5.2: an in-process Python peer is tested separately in AT-P12.
#[test]
#[ignore = "fake daemon child process, launched by facade tests"]
fn fake_daemon_child() {
    let Ok(knobs_json) = std::env::var("FBUILD_FACADE_DAEMON_KNOBS") else {
        return;
    };
    let knobs: serde_json::Value = serde_json::from_str(&knobs_json).unwrap();
    let millis = |key: &str| Duration::from_millis(knobs[key].as_u64().unwrap_or(0));
    let knobs = DaemonKnobs {
        attach_delay: millis("attach_delay_ms"),
        ack_delay: millis("ack_delay_ms"),
        reset_delay: millis("reset_delay_ms"),
        reset_success: knobs["reset_success"].as_bool().unwrap_or(false),
        emit_boot_line: knobs["emit_boot_line"].as_bool().unwrap_or(false),
        fail_first_write: knobs["fail_first_write"].as_bool().unwrap_or(false),
    };
    let port_file = std::env::var("FBUILD_FACADE_DAEMON_PORT_FILE").unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        std::fs::write(port_file, listener.local_addr().unwrap().port().to_string()).unwrap();
        accept_dual(listener, knobs).await;
    });
}

struct FakeDaemonGuard {
    child: std::process::Child,
    event_file: std::path::PathBuf,
    _temp_dir: tempfile::TempDir,
    _env_guard: std::sync::MutexGuard<'static, ()>,
}

impl FakeDaemonGuard {
    fn saw_teardown(&self) -> bool {
        std::fs::read_to_string(&self.event_file)
            .is_ok_and(|events| events.contains("detach") || events.contains("close"))
    }
}

fn assert_daemon_saw_teardown(daemon: &FakeDaemonGuard) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !daemon.saw_teardown() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        daemon.saw_teardown(),
        "fake daemon did not observe detach or WebSocket close"
    );
}

impl Drop for FakeDaemonGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        std::env::remove_var("FBUILD_DAEMON_PORT");
    }
}

fn start_fake_daemon(
    knobs: DaemonKnobs,
) -> (
    u16,
    std::process::Child,
    tempfile::TempDir,
    std::path::PathBuf,
) {
    let port_dir = tempfile::tempdir().unwrap();
    let port_file = port_dir.path().join("port");
    let event_file = port_dir.path().join("events");
    let knobs_json = serde_json::json!({
        "attach_delay_ms": knobs.attach_delay.as_millis() as u64,
        "ack_delay_ms": knobs.ack_delay.as_millis() as u64,
        "reset_delay_ms": knobs.reset_delay.as_millis() as u64,
        "reset_success": knobs.reset_success,
        "emit_boot_line": knobs.emit_boot_line,
        "fail_first_write": knobs.fail_first_write,
    });
    // allow-direct-spawn: integration test runs its own binary as a fake daemon child.
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "fake_daemon_child", "--nocapture"])
        .env("FBUILD_FACADE_DAEMON_KNOBS", knobs_json.to_string())
        .env("FBUILD_FACADE_DAEMON_PORT_FILE", &port_file)
        .env("FBUILD_FACADE_DAEMON_EVENT_FILE", &event_file)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .expect("start fake daemon child");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(contents) = std::fs::read_to_string(&port_file) {
            if let Ok(port) = contents.parse::<u16>() {
                return (port, child, port_dir, event_file);
            }
        }
        if let Ok(Some(status)) = child.try_wait() {
            panic!("fake daemon child exited before binding: {status}");
        }
        assert!(
            std::time::Instant::now() < deadline,
            "fake daemon child did not publish its port"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// `FBUILD_DAEMON_PORT` is process-global env state, so tests that touch it
/// must not run concurrently with each other. Every test in this file
/// serializes on this lock before setting the var and starting its daemon.
static DAEMON_PORT_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Starts a fake daemon and points `FBUILD_DAEMON_PORT` at it. Returns the
/// port plus the env lock guard (held for the caller's whole test body).
fn start_daemon_and_set_env(knobs: DaemonKnobs) -> (u16, FakeDaemonGuard) {
    let guard = DAEMON_PORT_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (port, child, temp_dir, event_file) = start_fake_daemon(knobs);
    std::env::set_var("FBUILD_DAEMON_PORT", port.to_string());
    (
        port,
        FakeDaemonGuard {
            child,
            event_file,
            _temp_dir: temp_dir,
            _env_guard: guard,
        },
    )
}

fn init_python() {
    use _native::_native;
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        pyo3::append_to_inittab!(_native);
        // `Python::attach` auto-initializes the interpreter with the
        // `auto-initialize` dev-dependency feature enabled (see Cargo.toml).
    });
}

/// Runs `code` (already containing its own `faulthandler.dump_traceback_later`
/// call, per §5.3) and turns a Python exception into a Rust panic with the
/// traceback printed to stderr.
fn run_snippet(py: pyo3::Python<'_>, code: &str) {
    py.run(&std::ffi::CString::new(code).unwrap(), None, None)
        .unwrap_or_else(|e| {
            e.print(py);
            panic!("Python snippet failed");
        });
}

#[path = "python_facades/cases.rs"]
mod cases;
#[path = "python_facades/extended.rs"]
mod extended;
