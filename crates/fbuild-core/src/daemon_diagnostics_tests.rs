//! Tests for daemon-abort attribution (FastLED/fbuild#1866).
//!
//! The report text is the one the embedded backend actually produced in
//! production; only the cache path prefix is neutralised.

use super::daemon_diagnostics::*;
use std::time::Duration;

const BUDGET_NS: u64 = 15 * 1_000_000_000;

fn report(sites: &str) -> String {
    format!("zccache: lock hold exceeded its budget. Registered sites:\n{sites}")
}

/// The flush-guard overrun from the original report: one overdue holder.
fn flush_guard_report() -> String {
    let age = BUDGET_NS + 25_622_665;
    report(&format!(
        "  Holding Mutex ... age_ns={age} budget_ns={BUDGET_NS} at \
         /proj/cache/zccache-1.15.8/src/artifact/store.rs:492:14 (thread tokio-rt-worker)\n"
    ))
}

/// The spawn-admission overrun: an overdue reader with writers queued behind
/// it, which is the shape that distinguishes "slow holder" from "just busy".
fn spawn_guard_report() -> String {
    let held = BUDGET_NS + 135_884_870;
    report(&format!(
        "  Holding RwLock(read) ... age_ns={held} budget_ns={BUDGET_NS} at \
         /proj/cache/zccache-1.15.8/src/daemon_core/daemon/spawn_exclusion.rs:61:64 \
         (thread running-process-actor)\n\
         \x20 Waiting RwLock(read) ... age_ns=900000000 budget_ns={BUDGET_NS} at \
         /proj/cache/zccache-1.15.8/src/daemon_core/daemon/spawn_exclusion.rs:61:64 \
         (thread running-process-actor)\n\
         \x20 Waiting RwLock(write) ... age_ns=800000000 budget_ns={BUDGET_NS} at \
         /proj/cache/zccache-1.15.8/src/daemon_core/daemon/server/persist/write_cached.rs:182:34 \
         (thread tokio-rt-worker)\n"
    ))
}

#[test]
fn flush_guard_overrun_is_attributed_to_the_cache_watchdog() {
    let loss = diagnose(&flush_guard_report());
    match &loss {
        DaemonLoss::WatchdogAbort { holders, waiters } => {
            assert_eq!(holders.len(), 1, "one overdue holder: {holders:?}");
            assert_eq!(*waiters, 0);
            let holder = &holders[0];
            assert_eq!(holder.kind, "Mutex");
            assert_eq!(holder.phase, LockPhase::Holding);
            assert_eq!(holder.budget, Duration::from_nanos(BUDGET_NS));
            assert!(holder.overran(), "reported age must exceed the budget");
            assert_eq!(holder.site, "artifact/store.rs:492:14");
            assert_eq!(holder.thread, "tokio-rt-worker");
        }
        other => panic!("expected a watchdog abort, got {other:?}"),
    }
}

#[test]
fn queued_waiters_are_counted_but_never_blamed() {
    let loss = diagnose(&spawn_guard_report());
    match &loss {
        DaemonLoss::WatchdogAbort { holders, waiters } => {
            assert_eq!(holders.len(), 1, "only the reader HELD the guard");
            assert_eq!(holders[0].kind, "RwLock(read)");
            assert_eq!(holders[0].site, "daemon/spawn_exclusion.rs:61:64");
            assert_eq!(*waiters, 2, "both queued guards are reported as waiters");
        }
        other => panic!("expected a watchdog abort, got {other:?}"),
    }
}

#[test]
fn a_watchdog_tail_with_a_build_log_around_it_still_classifies() {
    let tail = format!(
        "2026-01-01T00:00:00Z INFO fbuild-daemon: compile job started\n\
         2026-01-01T00:00:30Z INFO fbuild::output: compiling library.cpp\n{}",
        flush_guard_report()
    );
    assert!(matches!(diagnose(&tail), DaemonLoss::WatchdogAbort { .. }));
}

#[test]
fn the_last_report_block_wins() {
    let tail = format!(
        "{}\n2026-01-01T00:05:00Z INFO fbuild-daemon: recovered\n{}",
        flush_guard_report(),
        spawn_guard_report()
    );
    match diagnose(&tail) {
        DaemonLoss::WatchdogAbort { holders, .. } => {
            assert_eq!(
                holders[0].site, "daemon/spawn_exclusion.rs:61:64",
                "the newest block must decide the verdict"
            );
        }
        other => panic!("expected a watchdog abort, got {other:?}"),
    }
}

#[test]
fn ordinary_logs_are_not_misattributed() {
    let tail = "2026-01-01T00:00:00Z INFO fbuild-daemon: listening on 127.0.0.1:1\n\
                2026-01-01T00:00:01Z INFO fbuild::output: build succeeded\n";
    assert_eq!(diagnose(tail), DaemonLoss::Unattributed);
    assert_eq!(diagnose(""), DaemonLoss::Unattributed);
}

#[test]
fn a_redacted_age_does_not_fabricate_a_holder() {
    // The log pipeline redacts long digit runs; a report whose numbers were
    // replaced must degrade to the signature-only verdict, never to a
    // confident "held for 0s".
    let redacted = report(
        "  Holding Mutex ... age_ns=<redacted> budget_ns=<redacted> at \
         /proj/cache/zccache-1.15.8/src/artifact/store.rs:492:14 (thread tokio-rt-worker)\n",
    );
    match diagnose(&redacted) {
        DaemonLoss::WatchdogAbort { holders, .. } => {
            assert!(holders.is_empty(), "unparseable numbers: {holders:?}");
        }
        other => panic!("the signature line alone is still an abort: {other:?}"),
    }
}

#[test]
fn a_panic_is_reported_as_a_panic() {
    let tail = "2026-01-01T00:00:00Z INFO fbuild-daemon: starting\n\
                thread 'tokio-rt-worker' panicked at crates/fbuild-daemon/src/ctx.rs:12:5:\n\
                assertion failed: slots > 0\n";
    match diagnose(tail) {
        DaemonLoss::Panic { message } => {
            assert_eq!(message, "crates/fbuild-daemon/src/ctx.rs:12:5:");
        }
        other => panic!("expected a panic verdict, got {other:?}"),
    }
}

#[test]
fn explanation_names_the_issue_and_the_resource_it_could_not_reach() {
    let loss = diagnose(&flush_guard_report());
    let text = loss.explain(std::path::Path::new("/proj/root/daemon/daemon.log"));
    assert!(text.contains("FastLED/fbuild#1866"), "{text}");
    assert!(text.contains("artifact/store.rs:492:14"), "{text}");
    assert!(
        text.contains("not a device, firmware, upload or build error"),
        "{text}"
    );
    assert!(text.contains("/proj/root/daemon/daemon.log"), "{text}");
    // A retry must be the prescribed action, not a cache reset the user has to
    // guess at.
    assert!(text.contains("must be retried"), "{text}");
}

#[test]
fn tail_reading_is_bounded_and_keeps_the_newest_lines() {
    let dir = std::env::temp_dir().join(format!("fbuild-diag-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("daemon.log");
    // The abort report is the OLD part of this log; a bounded tail must not
    // resurrect it after newer ordinary lines.
    let mut body = flush_guard_report();
    for n in 0..40 {
        body.push_str(&format!(
            "2026-01-01T00:10:{n:02}Z INFO fbuild: build step {n}\n"
        ));
    }
    std::fs::write(&path, &body).expect("write log");

    let whole = read_log_tail(&path, 1024 * 1024).expect("read tail");
    assert!(
        whole.contains("Holding Mutex"),
        "a small file is read whole"
    );
    assert!(matches!(diagnose(&whole), DaemonLoss::WatchdogAbort { .. }));

    let bounded = read_log_tail(&path, 200).expect("read bounded tail");
    assert!(bounded.len() <= 200, "bounded: {}", bounded.len());
    assert!(
        !bounded.contains("Holding Mutex"),
        "the oldest bytes are dropped"
    );
    assert!(matches!(diagnose(&bounded), DaemonLoss::Unattributed));

    assert!(read_log_tail(&dir.join("absent.log"), 200).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
