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
use fbuild_core::platform::process::{
    Termination, pid_exe_stem_matches, pid_is_alive, terminate_pid, wait_for_pid_exit,
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
    home.join(fbuild_paths::FBUILD_DIR_NAME)
        .join("dev")
        .join("daemon")
        .join("root-owner.lock")
}

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("local addr").port()
}

fn cli_command(args: &[&str], port: u16, cache_dir: &std::path::Path) -> Command {
    let bin = std::env::var("CARGO_BIN_EXE_fbuild").expect("fbuild binary path");
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
            let _ = terminate_pid(pid, Termination::Force);
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
#[expect(clippy::too_many_lines, reason = "baseline, zackees/ci.yml#229")]
fn client_recovers_after_daemon_is_killed_uncleanly() {
    let Some(home) = real_home() else {
        eprintln!("skip: no home directory resolvable");
        return;
    };

    // The production spawn path resolves the daemon binary as a sibling of
    // the CLI. Under `cargo test --workspace` (and any full build) it exists;
    // under an isolated `-p fbuild-cli` test run it may not — skip then.
    let cli =
        NormalizedPath::new(std::env::var("CARGO_BIN_EXE_fbuild").expect("fbuild binary path"));
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
    terminate_pid(old_pid, Termination::Force).expect("terminate old daemon");
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
    terminate_pid(new_pid, Termination::Force).expect("terminate replacement daemon");
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

// ---------------------------------------------------------------
// #1329 AC5 — the daemon survives a REAL full flash (HIL).
//
// The defect in #1329 was that a Windows full flash through the external
// `esptool` child made the daemon disappear without diagnostics. AC1-AC4 are
// covered by the deterministic containment tests in `fbuild-deploy`
// (`esp32::deployer_containment_tests`); the remaining proof needs real
// hardware: the daemon that owns the deploy must be the SAME pid afterwards,
// and the client must get an explicit success/failure.
//
// Ignored because it needs a board and a real toolchain build. Run it with:
//
//   FBUILD_HIL_PROJECT_DIR=tests/platform/esp32c6 \
//   FBUILD_HIL_PORT=/dev/ttyACM0 \
//   FBUILD_HIL_ENVIRONMENT=esp32c6 \
//     soldr cargo test -p fbuild-cli --test it \
//       daemon_crash_recovery::daemon_survives_full_flash -- --ignored --nocapture
//
// `FBUILD_HIL_PORT` may be a node path (`COM9`, `/dev/ttyACM0`) or the stable
// `ser=<usb serial>` selector — `ser=` is what survives the USB renumbering a
// flash+reset normally causes (see #1329).
//
// Isolation contract, deliberately different from the crash-recovery test: the
// deploy runs against the AMBIENT cache root so a warm toolchain cache is
// reused (a cold espressif32 cache is a multi-GB download). Only the daemon
// ENDPOINT is isolated (`FBUILD_DAEMON_PORT`), so this test can never talk to a
// daemon it did not spawn, while the cache behaves exactly as in a user's
// deploy. The test therefore skips when another daemon already owns that root,
// and it always stops the daemon it spawned.
// ---------------------------------------------------------------

/// What the client said happened, classified from its exit status and output.
/// `Missing` is the #1329 failure mode: the client reported neither a success
/// nor a diagnosable failure.
#[derive(Debug, PartialEq, Eq)]
enum ClientOutcome {
    /// `deploy succeeded (<kind>)` / esptool's digest line.
    Success(String),
    /// A diagnosable failure: the message names a cause.
    Failure(String),
    /// No recognizable outcome — the AC5 contract is violated.
    Missing,
}

/// Markers a full flash leaves behind on success (the daemon's own
/// `deploy succeeded (...)` line, plus esptool's digest line when the client
/// streams it).
const SUCCESS_MARKERS: [&str; 2] = ["deploy succeeded", "hash of data verified"];

/// Markers that make an unsuccessful exit DIAGNOSABLE rather than silent.
/// #1329 AC3: the client must preserve the child exit status, a bounded
/// stream, or an explicit timeout/cancellation reason.
const FAILURE_MARKERS: [&str; 6] = [
    "deploy failed",
    "daemon error:",
    "error:",
    "timed out",
    "exit status",
    "verification failed",
];

/// First non-blank line containing any marker (ASCII case-insensitive).
fn first_line_containing<'a>(text: &'a str, markers: &[&str]) -> Option<&'a str> {
    text.lines().map(str::trim).find(|line| {
        !line.is_empty() && {
            let lowered = line.to_ascii_lowercase();
            markers.iter().any(|marker| lowered.contains(*marker))
        }
    })
}

/// Classify a deploy's client-side result. Pure, so it is unit-tested without
/// hardware: a green exit with no success line is as much a contract
/// violation as a red exit with no explanation.
fn classify_deploy_output(exit_ok: bool, output: &str) -> ClientOutcome {
    if exit_ok {
        return match first_line_containing(output, &SUCCESS_MARKERS) {
            Some(line) => ClientOutcome::Success(line.to_string()),
            None => ClientOutcome::Missing,
        };
    }
    match first_line_containing(output, &FAILURE_MARKERS) {
        Some(line) => ClientOutcome::Failure(line.to_string()),
        None => ClientOutcome::Missing,
    }
}

/// Isolated-endpoint CLI invocation: only `FBUILD_DAEMON_PORT` is pinned, so
/// the daemon that answers is the one this test spawned while the cache root
/// stays ambient. Output is stdout+stderr concatenated, because the daemon
/// streams tool output on both.
fn run_cli_hil(args: &[&str], port: u16) -> (bool, String) {
    let bin = std::env::var("CARGO_BIN_EXE_fbuild").expect("fbuild binary path");
    // allow-direct-spawn: test driver invoking the fbuild CLI binary under test.
    let output = std::process::Command::new(bin)
        .args(args)
        .env("FBUILD_DAEMON_PORT", port.to_string())
        .output()
        .expect("spawn fbuild CLI");
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.success(), combined)
}

/// Unwind-safe teardown for the daemon this test spawned (identified by its
/// isolated port). Mirrors `TestDaemonGuard`, but deliberately does NOT pin a
/// cache dir — the daemon lives on the ambient root.
struct HilDaemonGuard {
    port: u16,
    armed: bool,
}

impl HilDaemonGuard {
    fn new(port: u16) -> Self {
        Self { port, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for HilDaemonGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let (ok, status) = run_cli_hil(&["daemon", "status"], self.port);
        if !ok {
            return;
        }
        let Some(pid) = parse_status_pid(&status) else {
            return;
        };
        if !pid_is_alive(pid) {
            return;
        }
        let _ = run_cli_hil(&["daemon", "stop"], self.port);
        if wait_for_pid_exit(pid, Duration::from_secs(15)) {
            return;
        }
        if pid_exe_stem_matches(pid, DAEMON_EXE_STEM) {
            let _ = terminate_pid(pid, Termination::Force);
            let _ = wait_for_pid_exit(pid, Duration::from_secs(15));
        }
    }
}

#[test]
fn classify_deploy_output_requires_an_explicit_outcome() {
    assert_eq!(
        classify_deploy_output(
            true,
            "build ok\ndeploy succeeded (full flash); FBUILD_DEPLOY_PORT=x"
        ),
        ClientOutcome::Success("deploy succeeded (full flash); FBUILD_DEPLOY_PORT=x".to_string())
    );
    assert_eq!(
        classify_deploy_output(true, "Wrote 2469437 bytes\nHash of data verified."),
        ClientOutcome::Success("Hash of data verified.".to_string())
    );
    // A green exit with no described outcome is the silent-path defect.
    assert_eq!(
        classify_deploy_output(true, "build ok"),
        ClientOutcome::Missing
    );
    assert_eq!(
        classify_deploy_output(false, "build ok"),
        ClientOutcome::Missing
    );
    assert_eq!(
        classify_deploy_output(
            false,
            "deploy failed: esptool exited with exit status 2\nstderr: could not open port"
        ),
        ClientOutcome::Failure("deploy failed: esptool exited with exit status 2".to_string())
    );
    assert_eq!(
        classify_deploy_output(false, "daemon error: daemon did not become healthy"),
        ClientOutcome::Failure("daemon error: daemon did not become healthy".to_string())
    );
    // Case-insensitive, and blank lines never match.
    assert_eq!(
        classify_deploy_output(false, "\n\nflash TIMED OUT after 120s\n"),
        ClientOutcome::Failure("flash TIMED OUT after 120s".to_string())
    );
}

/// Start a daemon through the production acquisition path on the isolated
/// endpoint and return its pid. Fails loudly: a HIL run that cannot bring a
/// daemon up has proven nothing.
fn hil_start_daemon(port: u16) -> u32 {
    let (ok, start_log) = run_cli_hil(&["daemon", "restart"], port);
    assert!(ok, "`fbuild daemon restart` must succeed:\n{start_log}");
    assert!(
        wait_for_health(port, Duration::from_secs(60)),
        "daemon never became healthy on port {port}"
    );
    let (ok, status) = run_cli_hil(&["daemon", "status"], port);
    assert!(ok, "daemon status must succeed before the flash:\n{status}");
    let pid = parse_status_pid(&status).unwrap_or_else(|| panic!("no PID before flash:\n{status}"));
    assert!(pid_is_alive(pid), "daemon must be alive before the flash");
    pid
}

/// AC5: the SAME daemon served the whole flash and still answers afterwards.
fn hil_assert_daemon_survived(port: u16, pid_before: u32) -> u32 {
    let (ok, status_after) = run_cli_hil(&["daemon", "status"], port);
    assert!(
        ok,
        "daemon status must succeed after the flash — the daemon did not survive (#1329):\n{status_after}"
    );
    let pid_after = parse_status_pid(&status_after)
        .unwrap_or_else(|| panic!("no PID after flash:\n{status_after}"));
    assert_eq!(
        pid_after, pid_before,
        "#1329 AC5: daemon pid changed across the flash ({pid_before} -> {pid_after})"
    );
    assert!(
        pid_is_alive(pid_after),
        "daemon pid {pid_after} is not alive after the flash"
    );
    pid_after
}

/// `--ignored` HIL proof for #1329 AC5 on a real board.
#[test]
#[ignore = "requires a real board + toolchain build — set FBUILD_HIL_PROJECT_DIR and FBUILD_HIL_PORT"]
fn daemon_survives_full_flash() {
    let Some(project) = std::env::var("FBUILD_HIL_PROJECT_DIR").ok() else {
        eprintln!(
            "skip: set FBUILD_HIL_PROJECT_DIR (sketch dir) and FBUILD_HIL_PORT (node path or ser=<serial>)"
        );
        return;
    };
    let Some(device_port) = std::env::var("FBUILD_HIL_PORT").ok() else {
        eprintln!("skip: FBUILD_HIL_PROJECT_DIR is set but FBUILD_HIL_PORT is not");
        return;
    };
    let environment =
        std::env::var("FBUILD_HIL_ENVIRONMENT").unwrap_or_else(|_| "esp32c6".to_string());
    assert!(
        std::path::Path::new(&project).is_dir(),
        "FBUILD_HIL_PROJECT_DIR {project} is not a directory"
    );

    // The spawned daemon takes the ambient root in this process's mode; refuse
    // to displace a daemon we do not own.
    let lock_path = fbuild_paths::daemon_ownership::root_owner_lock_path();
    match RootOwnershipGuard::try_acquire_at(lock_path.as_path()) {
        Ok(Some(guard)) => drop(guard),
        Ok(None) => {
            eprintln!(
                "skip: a live daemon already owns {lock_path:?}; stop it (or run in the other mode)"
            );
            return;
        }
        Err(err) => {
            eprintln!("skip: cannot probe {lock_path:?}: {err}");
            return;
        }
    }

    let port = free_port();
    let mut daemon_guard = HilDaemonGuard::new(port);
    let pid_before = hil_start_daemon(port);

    // THE EXACT DEPLOY: full build + full flash through the daemon.
    eprintln!(
        "[hil] deploying {project} -e {environment} -p {device_port} (daemon pid {pid_before})"
    );
    let started = Instant::now();
    let (exit_ok, output) = run_cli_hil(
        &["deploy", &project, "-e", &environment, "-p", &device_port],
        port,
    );
    let elapsed = started.elapsed();
    eprintln!("[hil] deploy exit_ok={exit_ok} in {elapsed:?}");

    // AC5: the client's outcome is explicit — never a silent drop.
    let outcome = classify_deploy_output(exit_ok, &output);
    assert_ne!(
        outcome,
        ClientOutcome::Missing,
        "#1329 AC5: client reported no explicit outcome (exit_ok={exit_ok}) after {elapsed:?}:\n{output}"
    );
    eprintln!("[hil] client outcome: {outcome:?}");
    // Keep the tail visible in the HIL transcript: the pid assertion below is
    // only meaningful if the deploy really reached the flash tool, and the
    // transcript is the receipt.
    let tail: Vec<&str> = output.lines().rev().take(20).collect::<Vec<_>>();
    let tail: Vec<&str> = tail.into_iter().rev().collect();
    eprintln!("[hil] deploy output tail:\n{}", tail.join("\n"));

    let pid_after = hil_assert_daemon_survived(port, pid_before);

    // Teardown: stop the daemon we spawned and prove it exited, so the ambient
    // root is free for the next real deploy.
    let _ = run_cli_hil(&["daemon", "stop"], port);
    assert!(
        wait_for_pid_exit(pid_after, Duration::from_secs(30)),
        "daemon (pid {pid_after}) did not exit within 30s of `daemon stop`"
    );
    daemon_guard.disarm();
}
