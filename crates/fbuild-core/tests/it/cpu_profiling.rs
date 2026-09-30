//! On-CPU and off-CPU profiling, exercised end to end (FastLED/fbuild#1361).
//!
//! Two different questions, and fbuild needs both answered:
//!
//! - **on-CPU** — "what is running?" Sampled stacks, then symbolized off the
//!   hot path.
//! - **off-CPU** — "what is *waiting*?" This is the mode that matters most for
//!   `fbuild-daemon`, which spends nearly all its wall clock blocked on
//!   subprocesses, sockets, and the filesystem. A CPU profile is blind to all
//!   of that: a request that spent nine seconds waiting and one computing
//!   looks, in a CPU profile, like one second of work.
//!
//! These live in `fbuild-core` rather than next to the heap test in
//! `fbuild-daemon` for a linker reason, documented on the dev-dependency in
//! this crate's `Cargo.toml`: `running-process-probe` and the pinned zccache
//! disagree about `crash-handler`, and both export the same unmangled C
//! symbols, so no single binary can link both.

use std::time::Duration;

// ---------------------------------------------------------------------------
// On-CPU — sampled stacks plus symbolization
// ---------------------------------------------------------------------------

/// The frame the on-CPU profile has to find.
///
/// `#[inline(never)]` because the point is that this symbol survives into the
/// sampled stack; inlined into its caller it would have no address of its own
/// for samples to land on.
#[inline(never)]
fn fbuild_on_cpu_hot_loop(stop: &std::sync::atomic::AtomicBool) -> u64 {
    let mut acc = 0u64;
    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
        for i in 0..4096u64 {
            acc = acc.wrapping_add(i).wrapping_mul(31);
        }
        std::hint::black_box(acc);
    }
    acc
}

#[test]
fn on_cpu_profiling_samples_a_busy_thread_and_attributes_the_frames() {
    use running_process_probe_daemon::profile::session::{ProfileRequest, ProfileSession};
    use running_process_probe_daemon::profile::symbolize::ModuleResolver;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    // The sampler suspends *sibling* threads, so the work being profiled must
    // not sit on the thread calling `run()` — that thread is the profiler.
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = Arc::clone(&stop);
    let worker = std::thread::spawn(move || fbuild_on_cpu_hot_loop(&worker_stop));

    let session = ProfileSession::new(ProfileRequest {
        hz: 99,
        duration: Duration::from_millis(400),
    });
    let metrics = session.run();

    stop.store(true, Ordering::Relaxed);
    let acc = worker.join().expect("worker thread must not panic");
    std::hint::black_box(acc);

    assert!(
        metrics.samples_captured > 0,
        "a 400 ms session at 99 Hz over a busy thread must capture samples; \
         captured={} dropped={} threads_seen={}",
        metrics.samples_captured,
        metrics.samples_dropped,
        metrics.threads_seen
    );
    assert!(
        metrics.threads_seen >= 1,
        "at least the busy worker thread must appear in the profile"
    );

    // Symbolization. `ModuleResolver` is deliberately the floor of what can be
    // resolved without symbol files: it attributes each address to its owning
    // module plus an ASLR-independent offset, and stays honest about the rest
    // by naming unresolved frames `module+0xoffset` rather than inventing a
    // function name. Asserting DWARF/PDB function names here would be
    // asserting on the build's debug-info settings rather than on the
    // profiler; #1361 tracks that separately as the release-profile question.
    let mut resolver = ModuleResolver::for_current_process()
        .expect("module enumeration must work on a supported host");
    assert!(
        resolver.module_count() > 0,
        "the current process must have at least one loaded module"
    );
    let resolved = session.resolve(&mut resolver, metrics);

    let folded = resolved.folded();
    assert!(
        !folded.is_empty(),
        "resolved samples must fold into at least one stack"
    );
    let attributed = folded
        .iter()
        .flat_map(|(stack, _)| stack.iter())
        .any(|frame| !frame.is_empty());
    assert!(
        attributed,
        "every folded stack was empty — frames reached no module at all"
    );
}

// ---------------------------------------------------------------------------
// Off-CPU — the async/waiting pipeline
// ---------------------------------------------------------------------------

#[test]
fn off_cpu_profiling_ranks_waiting_above_running() {
    use running_process_probe_daemon::profile::async_profile::{
        CustomAdapter, TaskSample, profile, to_collapsed, to_pprof,
    };

    // Two tasks with inverted profiles: one waits nine seconds and works for
    // one, the other works constantly. In an on-CPU profile the busy task
    // dominates and the waiter is invisible. An off-CPU profile has to invert
    // that — surfacing the waiter is the entire reason to take one.
    let waiting = TaskSample {
        spawn_stack: vec![
            "fbuild_daemon::main".to_string(),
            "fbuild_daemon::handlers::operations::build".to_string(),
            "fbuild_build::compile_many::await_subprocess".to_string(),
        ],
        idle_nanos: 9_000_000_000,
        busy_nanos: 1_000_000_000,
        scheduled_nanos: 12_000,
        polls: 3,
        wakes: 3,
        name: "compile-wait".to_string(),
    };
    let busy = TaskSample {
        spawn_stack: vec![
            "fbuild_daemon::main".to_string(),
            "fbuild_daemon::status_manager::tick".to_string(),
        ],
        idle_nanos: 1_000_000,
        busy_nanos: 500_000_000,
        scheduled_nanos: 4_000,
        polls: 900,
        wakes: 900,
        name: "status-tick".to_string(),
    };

    let collected = {
        let samples = vec![waiting.clone(), busy.clone()];
        let mut adapter = CustomAdapter::new(move |_window| samples.clone());
        profile(&mut adapter, Duration::from_secs(5)).expect("adapter must produce samples")
    };
    assert_eq!(collected.len(), 2, "both tasks must survive collection");

    let pprof = to_pprof(&collected);
    assert!(
        !pprof.is_empty(),
        "off-CPU samples must serialize to a pprof profile"
    );

    let collapsed = to_collapsed(&collected);
    assert!(
        collapsed.contains("await_subprocess"),
        "the waiting task's spawn stack must appear in the off-CPU profile:\n{collapsed}"
    );

    // The ranking, not just the presence. Collapsed output opens on idle time,
    // so the waiter has to weigh more than the busy task even though the busy
    // task used 500x more CPU.
    let waiting_weight = collapsed_weight(&collapsed, "await_subprocess");
    let busy_weight = collapsed_weight(&collapsed, "status_manager::tick");
    assert!(
        waiting_weight > busy_weight,
        "off-CPU profile must rank waiting above running \
         (waiting={waiting_weight}, busy={busy_weight}):\n{collapsed}"
    );
}

/// Sum the counts of collapsed lines whose stack mentions `needle`.
///
/// Collapsed format is `frame;frame;frame <count>` per line.
fn collapsed_weight(collapsed: &str, needle: &str) -> u64 {
    collapsed
        .lines()
        .filter(|line| line.contains(needle))
        .filter_map(|line| line.rsplit_once(' '))
        .filter_map(|(_stack, count)| count.trim().parse::<u64>().ok())
        .sum()
}
