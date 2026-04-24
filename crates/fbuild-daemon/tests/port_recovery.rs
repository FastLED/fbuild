//! Integration test: daemon must recover from a crashed previous instance
//! without resorting to permissive Windows `SO_REUSEADDR` semantics.
//! See ISSUES.md "Issue B5a".
//!
//! This test is `#[ignore]` because it:
//!   1. Spawns the real `fbuild-daemon` binary (requires a build).
//!   2. Hard-kills processes (`taskkill /F` on Windows, `SIGKILL` on Unix).
//!   3. Allocates a dedicated port (18900) and leaves kernel TCP state
//!      lingering for whatever duration the OS chooses, which is not
//!      friendly to parallel CI runs.
//!
//! Run explicitly with:
//! ```bash
//! cargo test --release -p fbuild-daemon \
//!     daemon_rebinds_cleanly_after_hard_kill_with_open_connection \
//!     -- --ignored
//! ```
//!
//! ## What this test exposes
//!
//! With the current B5 mitigation (SO_REUSEADDR on the daemon listener)
//! this test passes for the *wrong reason* on Windows: SO_REUSEADDR there
//! permits multiple live binders, not just `TIME_WAIT` recovery. The
//! correct fix is graceful shutdown + `SO_LINGER 0` on accepted client
//! sockets + `SO_EXCLUSIVEADDRUSE` on the listener. Once that lands, this
//! test should still pass — but for the right reason.

use std::process::{Command, Stdio};
use std::time::Duration;

#[test]
#[ignore = "expects a real fbuild-daemon binary; run with --ignored"]
fn daemon_rebinds_cleanly_after_hard_kill_with_open_connection() {
    let port: u16 = 18900; // dedicated test port, avoids 8765 collisions
    let bin = env!("CARGO_BIN_EXE_fbuild-daemon");

    // 1) Spawn the first daemon.
    // allow-direct-spawn: test driver spawns the real fbuild-daemon binary under test.
    let mut d1 = Command::new(bin)
        .env("FBUILD_DAEMON_PORT", port.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn daemon #1");
    wait_for_health(port, Duration::from_secs(5));

    // 2) Open a long-lived TCP connection so the kernel has pending
    //    state when the daemon dies. We don't even need a WebSocket
    //    upgrade — a half-open TCP connection is enough to leave the
    //    socket in CLOSE_WAIT after kill -9.
    let _hold = std::net::TcpStream::connect(("127.0.0.1", port)).expect("hold connection");

    // 3) Hard-kill (no graceful shutdown).
    #[cfg(unix)]
    {
        // SAFETY: libc::kill is async-signal-safe and we are passing a PID
        // that we own. The kill syscall has no Rust-visible state to invalidate.
        unsafe {
            libc::kill(d1.id() as i32, libc::SIGKILL);
        }
    }
    #[cfg(windows)]
    {
        // taskkill /F is the Windows equivalent of SIGKILL.
        // allow-direct-spawn: test driver hard-killing the daemon process under test.
        let _ = Command::new("taskkill")
            .args(["/F", "/PID", &d1.id().to_string()])
            .status();
    }
    let _ = d1.wait();

    // 4) The kernel may still report the listener as LISTENING for
    //    a short window. Spawn a second daemon and assert it binds.
    //    With a *correct* fix (graceful shutdown + SO_EXCLUSIVEADDRUSE
    //    on Windows) this should succeed without permissive REUSEADDR.
    // allow-direct-spawn: test driver spawns the real fbuild-daemon binary under test.
    let mut d2 = Command::new(bin)
        .env("FBUILD_DAEMON_PORT", port.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn daemon #2");

    let healthy = try_health(port, Duration::from_secs(10));
    let _ = d2.kill();
    let _ = d2.wait();

    assert!(
        healthy,
        "second daemon failed to bind {} after hard-kill of first; \
         kernel left a dangling socket and our shutdown path is not \
         actually graceful (see ISSUES.md B5a).",
        port
    );
}

// --- helpers ---

fn try_health(port: u16, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if std::net::TcpStream::connect_timeout(
            &format!("127.0.0.1:{port}").parse().unwrap(),
            Duration::from_millis(200),
        )
        .is_ok()
        {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

fn wait_for_health(port: u16, timeout: Duration) {
    assert!(
        try_health(port, timeout),
        "daemon never became healthy on port {port}"
    );
}
