//! Unit tests for `daemon_stop`. Held in their own file so the module they
//! cover stays readable; `daemon_cmd` itself is at the 1000-LOC gate.

use super::*;

/// The case FastLED/fbuild#1360 reported: `/health` stops answering while
/// the process keeps running and keeps owning the port. `stop` used to
/// call this "not running" and exit 0, leaving a ~3.9 GB daemon alive to
/// fail every later build.
#[test]
fn a_silent_endpoint_with_a_live_process_is_terminated_not_declared_absent() {
    assert_eq!(
        plan_stop(false, Some(4321)),
        StopPlan::TerminateUnresponsive(4321)
    );
}

#[test]
fn a_silent_endpoint_with_no_live_process_is_nothing_to_stop() {
    assert_eq!(plan_stop(false, None), StopPlan::NothingToStop);
}

/// A healthy daemon is asked to leave rather than shot, even though its
/// PID is right there — a graceful exit releases serial ports and flushes
/// the log.
#[test]
fn a_healthy_endpoint_is_asked_to_stop() {
    assert_eq!(plan_stop(true, Some(4321)), StopPlan::AskDaemonToStop);
    assert_eq!(plan_stop(true, None), StopPlan::AskDaemonToStop);
}

/// A PID that never existed must never be reported as a stoppable daemon,
/// or `stop` would try to signal it. PID 0 is not a real target on either
/// OS family and is what `pid_is_alive` fails closed on.
#[test]
fn wait_for_process_exit_returns_immediately_for_a_dead_pid() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build runtime");
    let started = std::time::Instant::now();
    let exited = runtime.block_on(wait_for_process_exit(0, TERMINATION_BUDGET));
    assert!(exited, "PID 0 must read as gone");
    assert!(
        started.elapsed() < TERMINATION_BUDGET,
        "a dead PID must not burn the whole budget"
    );
}

/// The identity gate, exercised against this very process: `stop` must
/// refuse to signal a live PID whose image is not an fbuild-daemon, which
/// is what protects an unrelated process that inherited a recycled PID.
#[test]
fn a_live_non_daemon_pid_fails_the_identity_gate() {
    let me = std::process::id();
    assert!(
        fbuild_core::platform::process::pid_is_alive(me),
        "this test process must be alive"
    );
    assert!(
        !fbuild_core::platform::process::pid_exe_stem_matches(me, DAEMON_PROCESS_STEM),
        "the test binary is not an fbuild-daemon, so the gate must reject it"
    );
}
/// FastLED/fbuild#1627: after every spawn attempt fails, an unresponsive
/// daemon that still owns this endpoint's cache root is retired and retried
/// instead of failing the build outright.
#[test]
fn a_failed_spawn_retires_a_verified_unresponsive_daemon() {
    assert_eq!(
        plan_spawn_recovery(false, Some(4321)),
        SpawnRecovery::RetireAndRetry(4321)
    );
}

#[test]
fn a_failed_spawn_with_nothing_alive_to_retire_gives_up() {
    assert_eq!(plan_spawn_recovery(false, None), SpawnRecovery::GiveUp);
}

/// A daemon that answers is never retired, however the spawn attempts went:
/// it may be serving a concurrent build, and adopting it is what the failed
/// spawn was trying to do in the first place.
#[test]
fn a_healthy_endpoint_is_adopted_never_retired() {
    assert_eq!(plan_spawn_recovery(true, Some(4321)), SpawnRecovery::Adopt);
    assert_eq!(plan_spawn_recovery(true, None), SpawnRecovery::Adopt);
}
