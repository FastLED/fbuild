//! Integration test: `fbuild test-emu` must exit non-zero when the daemon
//! reports a failure. Regression for issue #130 where the CLI printed the
//! daemon error but then exited with code 0, so shells / CI treated
//! failures as success.
//!
//! Strategy: spin up a mock HTTP server on an ephemeral port that
//! pretends to be a healthy daemon, then returns a structured failure
//! for `POST /api/test-emu`. Point the CLI at that port via
//! `FBUILD_DAEMON_PORT` and assert the child process exit status is
//! non-zero.
//!
//! We do NOT spawn the real daemon; the mock is enough to exercise the
//! CLI's error-handling contract without dragging in toolchain/emulator
//! dependencies.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Run a `Command` with a hard wall-clock budget (FastLED/fbuild#806).
///
/// `Command::output()` blocks indefinitely if the spawned process wedges
/// (mock thread panics before responding, the CLI regresses to an infinite
/// loop, etc.). This helper spawns the child, polls `try_wait` until the
/// deadline, then force-kills if it exceeds the budget. Returns the
/// process output on clean exit, or an `Err(String)` describing the
/// timeout.
fn run_cli_with_timeout(mut cmd: Command, budget: Duration) -> Result<Output, String> {
    let mut child = cmd.spawn().expect("spawn child");
    let deadline = Instant::now() + budget;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child
                    .wait_with_output()
                    .map_err(|e| format!("wait_with_output: {e}"));
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "CLI child did not exit within {:.0}s — #806 timeout fired",
                        budget.as_secs_f64()
                    ));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("try_wait error: {e}")),
        }
    }
}

/// Minimal HTTP/1.1 request parser — reads headers, then the body based
/// on `Content-Length`. Keeps the test hermetic (no reqwest-server-side
/// dependency) and makes the response semantics impossible to
/// misinterpret. Only intended for loopback, fixed-shape requests from
/// the CLI under test.
fn read_request(stream: &mut TcpStream) -> (String, Vec<u8>) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .expect("read request line");
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).expect("read header line");
        if line == "\r\n" || line.is_empty() {
            break;
        }
        if let Some(rest) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            content_length = rest.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body).expect("read request body");
    }
    (request_line, body)
}

fn write_response(stream: &mut TcpStream, status_line: &str, body: &str) {
    let resp = format!(
        "{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status_line,
        body.len(),
        body
    );
    stream.write_all(resp.as_bytes()).ok();
    stream.flush().ok();
}

/// Spawn a mock HTTP daemon on an OS-assigned port. Returns the port.
/// The server handles:
/// - GET /health — 200 healthy (so `ensure_daemon_running` short-circuits).
/// - GET /api/daemon/info — 200 with `source_mtime=0` so the CLI does not
///   try to restart the "stale" daemon.
/// - POST /api/test-emu — 500 + structured OperationResponse JSON.
/// - POST /api/deploy — 500 + an actionable structured deploy failure.
fn spawn_mock_daemon(stop: Arc<AtomicBool>, cache_root: &std::path::Path) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    listener
        .set_nonblocking(true)
        .expect("set listener nonblocking");
    let port = listener.local_addr().expect("local_addr").port();

    let identity = fbuild_paths::running_process::DaemonCacheIdentity {
        mode: "prod",
        cache_root: cache_root.to_path_buf(),
        cache_root_key: fbuild_core::path::NormalizedPath::new(cache_root).display_slash(),
        cache_dir_source: "FBUILD_CACHE_DIR",
        trust_domain: fbuild_paths::running_process::LOCAL_TRUST_DOMAIN,
    };
    let info_body = serde_json::json!({
        "status": "healthy", "uptime_seconds": 1.0, "version": "test",
        "pid": 1, "port": port, "dev_mode": false, "operation_in_progress": false,
        "daemon_state": "idle", "current_operation": null, "client_count": 0,
        "spawner_cwd": null, "source_mtime": 0.0,
        "cache_identity": identity.label_value(),
        "cache_schema_version": fbuild_paths::running_process::CACHE_SCHEMA_VERSION,
        "long_path_support": fbuild_paths::long_paths::probe(),
    })
    .to_string();

    std::thread::spawn(move || {
        let healthy_body = "{\"status\":\"healthy\",\"uptime_seconds\":1.0,\"version\":\"test\",\"pid\":1,\"source_mtime\":0.0}";
        let fail_body = "{\"success\":false,\"request_id\":\"mock-1\",\"message\":\"mock daemon: simulated test-emu failure\",\"exit_code\":0,\"output_file\":null,\"output_dir\":null,\"launch_url\":null,\"stdout\":null,\"stderr\":null}";
        let deploy_fail_body = "{\"success\":false,\"request_id\":\"mock-2\",\"message\":\"deploy failed\",\"exit_code\":1,\"output_file\":null,\"output_dir\":null,\"launch_url\":null,\"stdout\":null,\"stderr\":\"RP2040 UF2 transfer failed with Windows error 121\"}";

        while !stop.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream
                        .set_nonblocking(false)
                        .expect("blocking per-connection");
                    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
                    let (request_line, _body) = read_request(&mut stream);
                    if request_line.starts_with("GET /health") {
                        write_response(&mut stream, "HTTP/1.1 200 OK", healthy_body);
                    } else if request_line.starts_with("GET /api/daemon/info") {
                        write_response(&mut stream, "HTTP/1.1 200 OK", &info_body);
                    } else if request_line.starts_with("POST /api/test-emu") {
                        // Return 500 with structured body + exit_code=0 to
                        // stress-test the CLI's fallback: it must still
                        // exit non-zero because success=false.
                        write_response(
                            &mut stream,
                            "HTTP/1.1 500 Internal Server Error",
                            fail_body,
                        );
                    } else if request_line.starts_with("POST /api/deploy") {
                        write_response(
                            &mut stream,
                            "HTTP/1.1 500 Internal Server Error",
                            deploy_fail_body,
                        );
                    } else {
                        write_response(&mut stream, "HTTP/1.1 404 Not Found", "{}");
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => break,
            }
        }
    });
    port
}

/// Minimal platformio.ini in a temp dir — enough for the CLI to form a
/// request, the mock daemon does not actually build anything.
fn make_test_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("platformio.ini"),
        "[env:uno]\nplatform = atmelavr\nboard = uno\nframework = arduino\n",
    )
    .expect("write platformio.ini");
    dir
}

fn make_test_cache() -> tempfile::TempDir {
    if fbuild_core::platform::host::is_windows() {
        tempfile::Builder::new()
            .prefix("fc-")
            .tempdir_in(fbuild_core::platform::fs::short_cache_base().unwrap())
            .unwrap()
    } else {
        tempfile::tempdir().unwrap()
    }
}

#[test]
fn deploy_output_survives_nonzero_exit() {
    let stop = Arc::new(AtomicBool::new(false));
    let cache = make_test_cache();
    let port = spawn_mock_daemon(Arc::clone(&stop), cache.path());

    let project = make_test_project();
    let bin = std::env::var("CARGO_BIN_EXE_fbuild").expect("fbuild binary path");

    // allow-direct-spawn: integration test driver that invokes the compiled fbuild binary.
    let mut cmd = Command::new(bin);
    cmd.args([
        "deploy",
        project.path().to_str().expect("utf-8 path"),
        "-e",
        "uno",
        "--skip-build",
    ])
    .env("FBUILD_DAEMON_PORT", port.to_string())
    .env("FBUILD_DEV_MODE", "0")
    .env("FBUILD_CACHE_DIR", cache.path())
    .env("RUNNING_PROCESS_DISABLE", "1")
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());

    let output = run_cli_with_timeout(cmd, Duration::from_secs(30)).expect("CLI wedged");

    stop.store(true, Ordering::Relaxed);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let code = output.status.code().unwrap_or(-1);

    assert_ne!(code, 0, "deploy failure must exit non-zero");
    assert!(
        stdout.contains("deploy failed"),
        "daemon failure message must survive process exit on stdout.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("RP2040 UF2 transfer failed with Windows error 121"),
        "actionable daemon failure must survive process exit on stderr.\nstdout: {stdout}\nstderr: {stderr}"
    );
}

#[test]
fn test_emu_exits_non_zero_when_daemon_returns_failure() {
    let stop = Arc::new(AtomicBool::new(false));
    let cache = make_test_cache();
    let port = spawn_mock_daemon(Arc::clone(&stop), cache.path());

    let project = make_test_project();
    let bin = std::env::var("CARGO_BIN_EXE_fbuild").expect("fbuild binary path");

    // Pin the CLI to this fixture's prod cache and HTTP endpoint, bypassing
    // broker adoption so the failure is supplied by this test server.
    // allow-direct-spawn: integration test driver that invokes the compiled fbuild binary.
    let mut cmd = Command::new(bin);
    cmd.args([
        "test-emu",
        project.path().to_str().expect("utf-8 path"),
        "-e",
        "uno",
        "--emulator",
        "simavr",
        "--timeout",
        "1",
    ])
    .env("FBUILD_DAEMON_PORT", port.to_string())
    .env("FBUILD_DEV_MODE", "0")
    .env("FBUILD_CACHE_DIR", cache.path())
    .env("RUNNING_PROCESS_DISABLE", "1")
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());

    // #806: cap the CLI invocation at 30 s. If the mock thread panics
    // before responding, the CLI's reqwest call has no top-level timeout
    // in this path — without this cap the test wedges indefinitely.
    let output = run_cli_with_timeout(cmd, Duration::from_secs(30)).expect("CLI wedged");

    stop.store(true, Ordering::Relaxed);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let code = output.status.code().unwrap_or(-1);

    // Regression for issue #130: exit code must be non-zero even when the
    // daemon returns success=false, exit_code=0.
    assert_ne!(
        code, 0,
        "CLI must exit non-zero on daemon failure.\nstdout: {}\nstderr: {}",
        stdout, stderr
    );
}
