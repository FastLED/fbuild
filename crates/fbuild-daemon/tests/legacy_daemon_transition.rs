//! Real-process regression test for the pre-#1159 legacy daemon transition
//! hazard (FastLED/fbuild#1159).
//!
//! Two things are exercised end-to-end with REAL child processes rather than
//! in-process fakes:
//!
//! 1. A stand-in for a pre-#1159 daemon: a real, long-lived child process
//!    whose executable stem is NOT `fbuild-daemon` gets its PID written into
//!    a temp-dir stand-in for the legacy stable pid file
//!    (`fbuild_paths::get_daemon_pid_file()`'s layout). Any code path that
//!    would read that PID and consider signalling it MUST refuse, because
//!    `fbuild_core::process_identity::pid_exe_stem_matches(pid,
//!    "fbuild-daemon")` fails closed on the stem mismatch. This test proves
//!    the gate stays closed and the process survives, and that liveness
//!    detection correctly flips to `false` once the process is actually
//!    killed (by the test itself, not through the gated path).
//!
//! 2. The REAL `fbuild-daemon` binary is spawned with an isolated temp
//!    HOME/USERPROFILE (+ `FBUILD_DEV_MODE=1`), and
//!    `fbuild_paths::daemon_ownership::RootOwnershipGuard::try_acquire_at`
//!    on its `root-owner.lock` is shown to be blocked while the daemon is
//!    alive and healthy, and acquirable again once the daemon process is
//!    killed — abrupt owner death releases ownership, exactly like an OS
//!    file lock is supposed to.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use fbuild_core::process_identity::{pid_exe_stem_matches, pid_is_alive};
use fbuild_paths::daemon_ownership::{DAEMON_EXE_STEM, RootOwnershipGuard};

/// Bounded `Child::wait()` (mirrors `tests/port_recovery.rs`): a missed
/// signal/kill must never hang the test suite.
fn wait_with_timeout(child: &mut Child, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => return false,
        }
    }
}

#[cfg(unix)]
fn hard_kill(child: &Child) {
    // SAFETY: kill(2) with a PID this test process owns (a child it spawned
    // itself). No signal handler runs in our own process; this only affects
    // the child.
    unsafe {
        libc::kill(child.id() as i32, libc::SIGKILL);
    }
}

#[cfg(windows)]
fn hard_kill(child: &Child) {
    // allow-direct-spawn: test driver hard-killing a process it spawned under test.
    let _ = Command::new("taskkill")
        .args(["/F", "/PID", &child.id().to_string()])
        .status();
}

/// Helper mode: NOT `fbuild-daemon` (this is the integration-test binary,
/// re-invoked as a subprocess), so the parent test has a real, long-lived,
/// non-daemon PID to probe. Runs until killed. `--ignored` because it must
/// never run as part of the normal suite — only the parent test below
/// spawns it explicitly.
#[test]
#[ignore = "subprocess helper for legacy_pid_stand_in_is_never_signaled (#1159)"]
fn subprocess_sleep_helper() {
    std::thread::sleep(Duration::from_secs(120));
}

#[test]
fn legacy_pid_stand_in_is_never_signaled() {
    let bin = std::env::current_exe().expect("current test exe");
    // allow-direct-spawn: test driver spawns its own test binary in helper mode.
    let mut helper = Command::new(&bin)
        .args([
            "--ignored",
            "--exact",
            "subprocess_sleep_helper",
            "--nocapture",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn non-daemon helper process");

    // Give it a moment to actually start running.
    let start_deadline = Instant::now() + Duration::from_secs(5);
    while !pid_is_alive(helper.id()) && Instant::now() < start_deadline {
        std::thread::sleep(Duration::from_millis(50));
    }

    let pid = helper.id();
    assert!(
        pid_is_alive(pid),
        "helper process must be alive after spawn"
    );

    // The exe-stem gate must fail closed: this PID is very much alive, but
    // its image is the *test* binary, not `fbuild-daemon` — any legacy pid
    // file / claim reader that found this PID must refuse to signal it.
    assert!(
        !pid_exe_stem_matches(pid, DAEMON_EXE_STEM),
        "a non-daemon process must never pass the fbuild-daemon exe-stem gate"
    );

    // Write the PID into a temp-dir stand-in for the legacy stable pid file
    // (`fbuild_paths::get_daemon_pid_file()`), mirroring what a pre-#1159
    // daemon (or an unrelated process that inherited a recycled PID) would
    // leave behind.
    let temp = tempfile::tempdir().expect("tempdir");
    let legacy_pid_file = temp.path().join("fbuild_daemon.pid");
    std::fs::write(&legacy_pid_file, pid.to_string()).expect("write legacy pid file");

    // Simulate the legacy-sweep read path: read the pid back, verify
    // liveness + exe stem BEFORE ever considering a signal. Because the
    // stem check fails, nothing here is allowed to call `terminate_pid`.
    let read_back: u32 = std::fs::read_to_string(&legacy_pid_file)
        .expect("read legacy pid file")
        .trim()
        .parse()
        .expect("pid file contains a u32");
    assert_eq!(read_back, pid);
    let should_signal = pid_is_alive(read_back) && pid_exe_stem_matches(read_back, DAEMON_EXE_STEM);
    assert!(
        !should_signal,
        "gate must refuse to signal a pid whose exe stem doesn't match fbuild-daemon"
    );

    // Positive proof: we did NOT call terminate_pid, so the process must
    // still be alive.
    assert!(
        pid_is_alive(pid),
        "helper process must remain alive — the exe-stem gate must have kept it un-signalled"
    );

    // Now kill it ourselves (test cleanup, NOT through the gated path) and
    // confirm liveness correctly flips to false afterward.
    hard_kill(&helper);
    let exited = wait_with_timeout(&mut helper, Duration::from_secs(15));
    assert!(
        exited,
        "helper process did not exit within 15s of hard-kill"
    );
    assert!(
        !pid_is_alive(pid),
        "helper process must be reported dead after being killed"
    );
    assert!(
        !pid_exe_stem_matches(pid, DAEMON_EXE_STEM),
        "gate must remain closed for a dead pid too (fails closed either way)"
    );
}

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("local addr").port()
}

/// Mirrors `fbuild_paths::get_fbuild_root()` / `get_daemon_dir()` /
/// `daemon_ownership::root_owner_lock_path()` layout (`<home>/.fbuild/dev/
/// daemon/root-owner.lock`) under an isolated HOME, computed WITHOUT
/// touching this test process's own environment — env vars are
/// process-global and the test suite runs multi-threaded, so mutating
/// `std::env` here would race other tests. The child `fbuild-daemon`
/// process gets `HOME`/`USERPROFILE` via `Command::env` instead, which is
/// scoped to just that child.
fn root_owner_lock_path_for(temp_home: &Path) -> PathBuf {
    temp_home
        .join(".fbuild")
        .join("dev")
        .join("daemon")
        .join("root-owner.lock")
}

async fn wait_for_http_health(port: u16, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    let url = format!("http://127.0.0.1:{port}/health");
    while Instant::now() < deadline {
        if let Ok(resp) = reqwest::get(&url).await {
            if resp.status().is_success() {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    false
}

#[tokio::test]
#[ignore = "spawns real fbuild-daemon (#1159)"]
async fn real_daemon_root_ownership_released_on_kill() {
    let temp_home = tempfile::tempdir().expect("temp home");
    let port = free_port();
    let bin = env!("CARGO_BIN_EXE_fbuild-daemon");
    let home_key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };

    // allow-direct-spawn: test driver spawns the real fbuild-daemon binary under test.
    let mut daemon = Command::new(bin)
        .env(home_key, temp_home.path())
        .env("FBUILD_DEV_MODE", "1")
        .env("FBUILD_DAEMON_PORT", port.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn real fbuild-daemon");

    let lock_path = root_owner_lock_path_for(temp_home.path());

    // Generous bound: cold zccache init under a fresh, isolated HOME can
    // take several seconds.
    let healthy = wait_for_http_health(port, Duration::from_secs(30)).await;
    assert!(healthy, "daemon never became healthy on port {port}");

    // By the time /health answers, main.rs has already passed root-
    // ownership acquisition: it happens before `CompileBackend::start()`,
    // which in turn happens before the router (and therefore `/health`)
    // is even constructed.
    let blocked = RootOwnershipGuard::try_acquire_at(&lock_path).expect("try_acquire_at io");
    assert!(
        blocked.is_none(),
        "root-owner.lock must be held by the live daemon"
    );

    hard_kill(&daemon);
    let exited = wait_with_timeout(&mut daemon, Duration::from_secs(30));
    assert!(exited, "daemon did not exit within 30s of hard-kill");

    // Abrupt owner death must release ownership — the OS releases the file
    // lock on process exit (including SIGKILL/TerminateProcess) — poll with
    // a deadline rather than assuming instantaneous release.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut reacquired = None;
    while Instant::now() < deadline {
        if let Ok(Some(guard)) = RootOwnershipGuard::try_acquire_at(&lock_path) {
            reacquired = Some(guard);
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(
        reacquired.is_some(),
        "root-owner.lock must become acquirable again after the owning daemon is killed"
    );
}
