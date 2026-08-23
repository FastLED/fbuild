//! `fbuild daemon stop`: deciding what to stop, and confirming it stopped.
//!
//! Split out of `daemon_cmd` so that file stays under the 1000-LOC gate. The
//! decision (`StopPlan`) is kept apart from the I/O on purpose — see its own
//! docs for why FastLED/fbuild#1360 needed a third state.

use super::*;

/// What `fbuild daemon stop` should do, decided from the two facts that
/// distinguish the cases. Kept separate from the I/O so the contract is
/// testable without a live daemon and without killing a real process.
///
/// # Why this exists (FastLED/fbuild#1360)
///
/// The previous code knew only two states: the endpoint answers, or there is
/// no daemon. A daemon that had grown to ~3.9 GB stopped answering `/health`
/// while its process kept running and kept owning the port — the third state.
/// `stop` read that as "not running", deleted the pid/port/claim records,
/// printed a success line and exited 0. The wedged process survived every
/// `stop` the user typed, and deleting the records made it *harder* to find
/// afterwards. A stop that reports success without stopping anything hides
/// this whole class of failure, so the outcome now follows the process, not
/// the endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopPlan {
    /// Nothing answers and no recorded fbuild-daemon process is alive. The
    /// records on disk are known-garbage and get swept.
    NothingToStop,
    /// The endpoint answers: ask it to leave gracefully, then confirm it did.
    AskDaemonToStop,
    /// The endpoint is silent but the recorded process is still alive.
    /// Unresponsive, not absent.
    TerminateUnresponsive(u32),
}

/// Decide what `stop` should do. `live_daemon_pid` is `Some` only when a
/// recorded PID is both alive *and* verified to be an fbuild-daemon image —
/// see [`recorded_daemon_pid`].
pub fn plan_stop(endpoint_healthy: bool, live_daemon_pid: Option<u32>) -> StopPlan {
    match (endpoint_healthy, live_daemon_pid) {
        // A healthy endpoint is a daemon that can still be asked politely.
        (true, _) => StopPlan::AskDaemonToStop,
        (false, Some(pid)) => StopPlan::TerminateUnresponsive(pid),
        (false, None) => StopPlan::NothingToStop,
    }
}

/// The recorded daemon PID, but only when it is alive *and* the process at
/// that PID is really an fbuild-daemon.
///
/// Fail-closed on identity on purpose: PIDs are recycled, and a stale pid
/// file plus an unlucky reuse would otherwise make `stop` terminate an
/// unrelated process. Unverifiable identity is treated as "not our daemon".
pub fn recorded_daemon_pid() -> Option<u32> {
    let pid = read_pid_from_file().ok()?;
    if !fbuild_core::platform::process::pid_is_alive(pid) {
        return None;
    }
    if !fbuild_core::platform::process::pid_exe_stem_matches(pid, DAEMON_PROCESS_STEM) {
        tracing::debug!(
            pid,
            "recorded daemon PID is alive but is not an fbuild-daemon image; treating the pid file as stale"
        );
        return None;
    }
    Some(pid)
}

/// Executable stem every daemon process is verified against before `stop`
/// signals it.
const DAEMON_PROCESS_STEM: &str = "fbuild-daemon";

/// How long a daemon gets to honour a graceful shutdown before `stop`
/// escalates. Matches the pre-existing 5 s wait; past that the daemon is not
/// shutting down, it is stuck.
const GRACEFUL_STOP_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

/// How long a signalled process gets to actually disappear. Termination is
/// asynchronous on both OS families, but a process that has not gone in 5 s
/// is not going.
const TERMINATION_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

/// `fbuild daemon stop` — stop the daemon for *this* endpoint and report what
/// actually happened.
///
/// Deliberately scoped to this endpoint's recorded PID rather than scanning
/// for every `fbuild-daemon` on the machine: daemons are keyed per backend
/// version + cache identity (FastLED/fbuild#1009), so another checkout's
/// daemon is a legitimate neighbour, not debris. `fbuild daemon kill-all` is
/// the global hammer; `stop` is not.
pub async fn run_daemon_stop(client: &DaemonClient) -> fbuild_core::Result<()> {
    let endpoint_healthy = client.health().await;
    match plan_stop(endpoint_healthy, recorded_daemon_pid()) {
        StopPlan::NothingToStop => {
            // FastLED/fbuild#1213 part 2: a crashed daemon's port/pid/status
            // records used to survive `daemon stop` indefinitely and every
            // later `daemon status` kept describing a dead PID. Nothing alive
            // is exactly when those records are known to be garbage.
            let removed = clear_daemon_records();
            if removed.is_empty() {
                output::result("daemon is not running");
            } else {
                output::result(format!(
                    "daemon is not running (cleared stale records: {})",
                    removed.join(", ")
                ));
            }
            Ok(())
        }
        StopPlan::TerminateUnresponsive(pid) => {
            output::warn(format!(
                "daemon at {} is not answering /health, but PID {} is still alive — terminating \
                 it (a wedged daemon keeps owning the port and fails every later build; see \
                 FastLED/fbuild#1360)",
                fbuild_paths::get_daemon_url(),
                pid
            ));
            terminate_and_confirm(pid).await?;
            clear_daemon_records();
            output::result(format!(
                "daemon stopped (terminated unresponsive PID {})",
                pid
            ));
            Ok(())
        }
        StopPlan::AskDaemonToStop => {
            // Ask the endpoint who it is *before* shutting it down; afterwards
            // there is nothing left to ask, and the PID is what the escalation
            // below needs.
            let pid = match client.daemon_info().await {
                Ok(info) => Some(info.pid),
                Err(_) => recorded_daemon_pid(),
            };
            client.shutdown().await?;

            let deadline = std::time::Instant::now() + GRACEFUL_STOP_BUDGET;
            while std::time::Instant::now() < deadline {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                if !client.health().await {
                    break;
                }
            }

            // A silent endpoint is not a dead process. When the PID is known,
            // the process is the thing that has to be gone — that gap is
            // exactly what let #1360's daemon survive a "successful" stop.
            if let Some(pid) = pid {
                // Give a graceful exit the rest of its budget before
                // escalating: unwinding a build can outlast the socket.
                if !wait_for_process_exit(pid, GRACEFUL_STOP_BUDGET).await {
                    output::warn(format!(
                        "daemon accepted the shutdown but PID {} is still alive after {}s — \
                         terminating it",
                        pid,
                        GRACEFUL_STOP_BUDGET.as_secs() * 2
                    ));
                    terminate_and_confirm(pid).await?;
                }
            } else if client.health().await {
                // No PID to follow and the endpoint still answers: report the
                // failure instead of the old cheerful "may still be shutting
                // down", which exited 0 on a daemon that never left.
                return Err(fbuild_core::FbuildError::DaemonError(format!(
                    "daemon at {} did not stop and its PID is unknown; run `fbuild daemon \
                     kill-all` to clear it",
                    fbuild_paths::get_daemon_url()
                )));
            }

            // The daemon removes its own pid/port/claim on a graceful exit;
            // sweep anything it left behind (notably daemon_status.json) so
            // `stop` always leaves a clean dir.
            clear_daemon_records();
            output::result("daemon stopped");
            Ok(())
        }
    }
}

/// Terminate `pid` and confirm the process is actually gone.
///
/// Escalates to a forced kill rather than starting with one: a graceful
/// terminate lets the daemon release serial ports and flush its log. On
/// Windows `taskkill` without `/F` is routinely *refused* for a windowless
/// console process, so escalation is the normal path there rather than an
/// anomaly — which is why the graceful attempt's exit status is ignored and
/// only the liveness check decides.
async fn terminate_and_confirm(pid: u32) -> fbuild_core::Result<()> {
    if let Err(error) = kill_process(pid, false).await {
        tracing::debug!(pid, %error, "graceful terminate refused; escalating to a forced kill");
    }
    if wait_for_process_exit(pid, TERMINATION_BUDGET).await {
        return Ok(());
    }

    kill_process(pid, true).await?;
    if wait_for_process_exit(pid, TERMINATION_BUDGET).await {
        return Ok(());
    }

    Err(fbuild_core::FbuildError::DaemonError(format!(
        "PID {} is still alive after a forced kill; it may be stuck in a kernel wait. Retry, or \
         terminate it from the OS process manager",
        pid
    )))
}

/// Poll until `pid` is gone or `budget` elapses. Returns whether it went.
async fn wait_for_process_exit(pid: u32, budget: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + budget;
    loop {
        if !fbuild_core::platform::process::pid_is_alive(pid) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

#[cfg(test)]
#[path = "daemon_stop_tests.rs"]
mod tests;
