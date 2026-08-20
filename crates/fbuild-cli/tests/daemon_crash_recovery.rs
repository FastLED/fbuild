//! Real-process regression test for FastLED/fbuild#1228 and #1320: after a daemon is
//! killed uncleanly (leaving stale port/pid/status records behind), the very
//! next CLI invocation must respawn a fresh daemon and reach it — the client
//! must never sit redialing the dead endpoint.
//!
//! This drives the REAL `fbuild` binary, which in turn spawns the REAL
//! sibling `fbuild-daemon` binary through the production spawn path
//! (`ensure_daemon_running` → sibling discovery → detached spawn). Isolation
//! notes, because the production spawn path rebuilds the daemon's
//! environment from the OS user baseline (`user_baseline_environment`
//! discards any test-provided `HOME`/`USERPROFILE`):
//!
//! - The daemon ALWAYS resolves the real `~/.fbuild/dev/` root. The test
//!   therefore refuses to run (skips) when that root's `root-owner.lock` is
//!   held — i.e. when a real dev daemon is alive on this machine — so it can
//!   never displace or corrupt a daemon it does not own.
//! - Port and cache are still isolated: `FBUILD_DAEMON_PORT` (a free
//!   ephemeral port) and `FBUILD_CACHE_DIR` (a tempdir) both survive the
//!   spawn path's `FBUILD_*` propagation filter.
//! - `RUNNING_PROCESS_DISABLE=1` pins the legacy direct acquisition path so
//!   the test deterministically exercises `ensure_direct_daemon_running`
//!   (the path traced in #1228) rather than broker adoption.

use std::process::Command;
use std::time::{Duration, Instant};

use fbuild_core::path::NormalizedPath;
use fbuild_core::process_identity::{
    pid_exe_stem_matches, pid_is_alive, terminate_pid, wait_for_pid_exit,
};
use fbuild_paths::daemon_ownership::{DAEMON_EXE_STEM, RootOwnershipGuard};

/// The real user home, the same way the spawned daemon will resolve it.
fn real_home() -> Option<NormalizedPath> {
    let key = if fbuild_core::platform::host::is_windows() {
        "USERPROFILE"
    } else {
        "HOME"
    };
    std::env::var_os(key).map(NormalizedPath::new)
}

/// `<real home>/.fbuild/dev/daemon/root-owner.lock` — the dev-mode root
/// ownership lock the spawned daemon will contend for. Mirrors
/// `fbuild_paths` layout without mutating this process's env (env vars are
/// process-global and tests run multi-threaded).
fn dev_root_owner_lock(home: &NormalizedPath) -> NormalizedPath {
    home.join(".fbuild")
        .join("dev")
        .join("daemon")
        .join("root-owner.lock")
}

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("local addr").port()
}

fn cli_command(args: &[&str], port: u16, cache_dir: &std::path::Path) -> Command {
    let bin = env!("CARGO_BIN_EXE_fbuild");
    // allow-direct-spawn: test driver invoking the fbuild CLI binary under test.
    let mut command = Command::new(bin);
    command
        .args(args)
        .env("FBUILD_DEV_MODE", "1")
        .env("FBUILD_DAEMON_PORT", port.to_string())
        .env("FBUILD_CACHE_DIR", cache_dir)
        .env("RUNNING_PROCESS_DISABLE", "1");
    command
}

/// Run the fbuild CLI with the test's isolation env; returns (exit ok, stdout).
fn run_cli(args: &[&str], port: u16, cache_dir: &std::path::Path) -> (bool, String) {
    let output = cli_command(args, port, cache_dir)
        .output()
        .expect("spawn fbuild CLI");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    (output.status.success(), stdout)
}

fn try_run_cli(args: &[&str], port: u16, cache_dir: &std::path::Path) -> Option<(bool, String)> {
    let output = cli_command(args, port, cache_dir).output().ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    Some((output.status.success(), stdout))
}

/// Unwind-safe cleanup for the detached daemon spawned by this test.
///
/// The isolated port identifies the test's daemon. Forced termination still
/// verifies the executable stem so a recycled PID can never be signalled.
struct TestDaemonGuard {
    port: u16,
    cache_dir: NormalizedPath,
    armed: bool,
}

impl TestDaemonGuard {
    fn new(port: u16, cache_dir: &std::path::Path) -> Self {
        Self {
            port,
            cache_dir: NormalizedPath::from(cache_dir),
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TestDaemonGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }

        let Some((status_ok, status)) =
            try_run_cli(&["daemon", "status"], self.port, &self.cache_dir)
        else {
            return;
        };
        if !status_ok {
            return;
        }
        let Some(pid) = parse_status_pid(&status) else {
            return;
        };
        if !pid_is_alive(pid) {
            return;
        }

        let _ = try_run_cli(&["daemon", "stop"], self.port, &self.cache_dir);
        if wait_for_pid_exit(pid, Duration::from_secs(15)) {
            return;
        }
        if pid_exe_stem_matches(pid, DAEMON_EXE_STEM) {
            terminate_pid(pid);
            let _ = wait_for_pid_exit(pid, Duration::from_secs(15));
        }
    }
}

/// Parse `  PID:     12345` out of `fbuild daemon status` output.
fn parse_status_pid(status_stdout: &str) -> Option<u32> {
    status_stdout.lines().find_map(|line| {
        let rest = line.trim().strip_prefix("PID:")?;
        rest.trim().parse().ok()
    })
}

fn wait_for_health(port: u16, budget: Duration) -> bool {
    let url = format!("http://127.0.0.1:{port}/health");
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if let Ok(resp) = reqwest::blocking::get(&url) {
            if resp.status().is_success() {
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

#[test]
#[ignore = "spawns real fbuild + fbuild-daemon binaries (#1228)"]
fn client_recovers_after_daemon_is_killed_uncleanly() {
    let Some(home) = real_home() else {
        eprintln!("skip: no home directory resolvable");
        return;
    };

    // The production spawn path resolves the daemon binary as a sibling of
    // the CLI. Under `cargo test --workspace` (and any full build) it exists;
    // under an isolated `-p fbuild-cli` test run it may not — skip then.
    let cli = NormalizedPath::new(env!("CARGO_BIN_EXE_fbuild"));
    let daemon_name = fbuild_core::platform::executable::name("fbuild-daemon", "fbuild-daemon.exe");
    let sibling = cli
        .parent()
        .map(|d| NormalizedPath::new(d).join(daemon_name));
    if !sibling.as_ref().is_some_and(|path| path.as_path().exists()) {
        eprintln!(
            "skip: no sibling fbuild-daemon binary at {sibling:?} — build fbuild-daemon first"
        );
        return;
    }

    // Never contend with a real dev daemon: the spawned daemon will use the
    // real ~/.fbuild/dev root (see module docs), so if something already owns
    // it, running this test would try to displace a daemon we don't own.
    let lock_path = dev_root_owner_lock(&home);
    match RootOwnershipGuard::try_acquire_at(lock_path.as_path()) {
        Ok(Some(guard)) => drop(guard), // free — safe to proceed
        Ok(None) => {
            eprintln!("skip: a live dev daemon holds {lock_path:?}");
            return;
        }
        Err(err) => {
            eprintln!("skip: cannot probe {lock_path:?}: {err}");
            return;
        }
    }

    let cache_dir = tempfile::tempdir().expect("temp cache dir");
    let port = free_port();
    let mut daemon_guard = TestDaemonGuard::new(port, cache_dir.path());

    // 1. Bring a daemon up through the production acquisition path.
    let (ok, _) = run_cli(&["daemon", "restart"], port, cache_dir.path());
    assert!(ok, "initial `fbuild daemon restart` must succeed");
    assert!(
        wait_for_health(port, Duration::from_secs(30)),
        "daemon never became healthy on port {port}"
    );
    let (ok, status) = run_cli(&["daemon", "status"], port, cache_dir.path());
    assert!(ok, "daemon status must succeed while daemon is up");
    let old_pid = parse_status_pid(&status)
        .unwrap_or_else(|| panic!("no PID in daemon status output:\n{status}"));
    assert!(
        pid_is_alive(old_pid),
        "freshly started daemon must be alive"
    );

    // 2. Crash it. This is the #1213/#1228 scenario: unclean death that
    //    leaves the port/pid/status records in place.
    terminate_pid(old_pid);
    assert!(
        wait_for_pid_exit(old_pid, Duration::from_secs(15)),
        "daemon (pid {old_pid}) did not die within 15s of terminate_pid"
    );

    // 3. The very next client invocation must recover: detect the dead
    //    endpoint, respawn, and reach the fresh daemon. `daemon restart`
    //    routes through the same `ensure_daemon_running` every build/deploy
    //    request uses.
    let (ok, _) = run_cli(&["daemon", "restart"], port, cache_dir.path());
    assert!(
        ok,
        "client invocation after unclean daemon death must respawn and succeed (#1228)"
    );
    assert!(
        wait_for_health(port, Duration::from_secs(30)),
        "respawned daemon never became healthy on port {port}"
    );
    let (ok, status) = run_cli(&["daemon", "status"], port, cache_dir.path());
    assert!(ok, "daemon status must succeed after recovery");
    let new_pid = parse_status_pid(&status)
        .unwrap_or_else(|| panic!("no PID in post-recovery status output:\n{status}"));
    assert_ne!(new_pid, old_pid, "recovery must have spawned a NEW daemon");
    assert!(pid_is_alive(new_pid), "respawned daemon must be alive");

    // 4. Kill the replacement and make `clean sketch` the next client.
    //    Unlike build/deploy, clean used to skip `ensure_daemon_running` and
    //    dial the stale endpoint directly (#1320).
    terminate_pid(new_pid);
    assert!(
        wait_for_pid_exit(new_pid, Duration::from_secs(15)),
        "replacement daemon (pid {new_pid}) did not exit within 15s"
    );

    let project = tempfile::tempdir().expect("temp clean project");
    std::fs::write(
        project.path().join("platformio.ini"),
        "[env:uno]\nplatform = atmelavr\nboard = uno\nframework = arduino\n",
    )
    .expect("write platformio.ini");
    let project_dir = project.path().to_string_lossy().into_owned();
    let (ok, stdout) = run_cli(
        &["clean", "sketch", &project_dir, "-e", "uno"],
        port,
        cache_dir.path(),
    );
    assert!(
        ok,
        "clean after daemon loss must respawn and succeed (#1320):\n{stdout}"
    );
    assert!(
        wait_for_health(port, Duration::from_secs(30)),
        "daemon respawned by clean never became healthy on port {port}"
    );
    let (ok, status) = run_cli(&["daemon", "status"], port, cache_dir.path());
    assert!(ok, "daemon status must succeed after clean recovery");
    let clean_pid = parse_status_pid(&status)
        .unwrap_or_else(|| panic!("no PID after clean recovery:\n{status}"));
    assert_ne!(clean_pid, new_pid, "clean recovery must spawn a NEW daemon");
    assert!(
        pid_is_alive(clean_pid),
        "clean-spawned daemon must be alive"
    );

    // 5. Cleanup: stop the daemon we spawned (also exercises the #1227
    //    stale-record clearing) and confirm it exits.
    let (ok, _) = run_cli(&["daemon", "stop"], port, cache_dir.path());
    assert!(ok, "daemon stop must succeed");
    assert!(
        wait_for_pid_exit(clean_pid, Duration::from_secs(15)),
        "daemon (pid {clean_pid}) did not exit within 15s of `daemon stop`"
    );
    daemon_guard.disarm();
}
