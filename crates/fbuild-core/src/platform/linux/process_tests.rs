//! FastLED/fbuild#1628: a contained child must outlive the thread that forked it.
//!
//! `PR_SET_PDEATHSIG` fires when the *thread* that forked the child exits, not
//! the process. The daemon forks compilers from Tokio threads that do not live
//! for the whole compile -- a worker that passes through `block_in_place` is
//! demoted to the blocking pool and retires after its idle keep-alive -- so a
//! long compile was SIGKILLed mid-run (exit -9, empty stderr) on CI.

use crate::platform::process::{init_containment, spawn_tokio_contained};

#[test]
fn contained_child_outlives_the_thread_that_spawned_it() {
    init_containment("fbuild-core-test").expect("containment initializes");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime builds");
    let handle = runtime.handle().clone();

    // Fork from a thread that exits straight afterwards, the way a retired
    // blocking-pool thread does while its compiler is still running.
    let mut child = std::thread::spawn(move || {
        let _entered = handle.enter();
        // allow-direct-spawn: containment regression test spawns through the contained path under test.
        let mut command = tokio::process::Command::new("sleep");
        command.arg("1");
        spawn_tokio_contained(&mut command).expect("child spawns")
    })
    .join()
    .expect("spawning thread joins");

    let status = runtime
        .block_on(child.wait())
        .expect("child is waitable");
    assert!(
        status.success(),
        "contained child was killed when its spawning thread exited: {status:?}"
    );
}

#[test]
fn resident_memory_of_the_current_process_is_reported() {
    let usage = super::resident_memory(std::process::id()).expect("current process resident set");
    assert!(usage.resident_bytes > 0);
    let peak = usage.peak_resident_bytes.expect("Linux reports VmHWM");
    assert!(peak >= usage.resident_bytes, "peak {peak} < rss {}", usage.resident_bytes);
}

#[test]
fn a_pid_without_a_proc_entry_reports_nothing() {
    // Above the kernel's default `pid_max` (4194304), so no `/proc` entry can
    // exist: the answer must be `None`, never a zero-sized working set.
    assert!(super::resident_memory(9_000_000).is_none());
}

#[test]
fn status_memory_lines_parse_as_kib() {
    assert_eq!(super::parse_status_kib("     1024 kB"), Some(1_048_576));
    assert_eq!(super::parse_status_kib("  0 kB"), Some(0));
    assert_eq!(super::parse_status_kib("     1024 MB"), None);
    assert_eq!(super::parse_status_kib(""), None);
}
