//! Sampled heap profiling, exercised end to end (FastLED/fbuild#1361).
//!
//! An integration test is its own final executable, so declaring the global
//! allocator here is not incidental — it *is* the downstream linkage contract
//! `fbuild-daemon` relies on. A library cannot choose its consumer's
//! allocator, so if this file did not compile, neither would the daemon.
//!
//! This is the mode #1360 needed and did not have when the daemon reached
//! ~3.9 GB resident while idle: *what is holding this memory?*
//!
//! The on-CPU and off-CPU halves of #1361 live in
//! `fbuild-core/tests/cpu_profiling.rs`, for a linker reason rather than a
//! taste one — see the dev-dependency comment in `fbuild-core/Cargo.toml`.

use mimalloc_pprof::MiMalloc;

#[global_allocator]
static ALLOC: MiMalloc = MiMalloc;

/// Stops the profiler even when an assertion unwinds.
///
/// The profiler is process-wide state; leaking it enabled would make anything
/// later in this binary run under a sampler it never asked for.
struct ProfilerGuard;

impl Drop for ProfilerGuard {
    fn drop(&mut self) {
        mimalloc_pprof::prof::stop();
    }
}

/// One test, not three, because the profiler is a process-wide singleton.
///
/// Cargo runs a test binary's cases on parallel threads, so two cases that
/// each start and stop the profiler race: whichever finishes first stops
/// sampling out from under the other, and the loser sees zero live samples.
/// Splitting these would buy nicer names at the cost of a test that fails
/// depending on thread scheduling — so the whole heap contract is asserted in
/// one place, in order.
#[test]
fn heap_profiling_captures_retained_allocations_and_dumps_them_as_pprof() {
    if mimalloc_pprof::prof::is_enabled() {
        mimalloc_pprof::prof::stop();
    }
    // Sample every byte. At the default 512 KiB rate these assertions would be
    // probabilistic, and a flaky proof of a profiler is worse than none.
    assert!(
        fbuild_daemon::heap_profile::start(1),
        "heap profiler must start when nothing else holds a session"
    );
    let _guard = ProfilerGuard;
    assert!(
        fbuild_daemon::heap_profile::is_enabled(),
        "the daemon helper must report the session it just started"
    );

    // Many small blocks rather than one big one, deliberately. A single
    // multi-megabyte `Vec` can be served by mimalloc's large-object path,
    // which does not always pass through the sampling hook — a version of
    // this test that allocated 4 MiB in one go passed or failed depending on
    // arena state. Retaining several thousand 4 KiB blocks is both stable and
    // the shape a real leak actually has: many live objects, not one giant
    // buffer. Same shape as running-process's own `mimalloc_leaker` fixture.
    //
    // Held to the end of the test on purpose: a freed allocation is exactly
    // what a *live* heap profile must not report, so retention is what makes
    // these assertions mean "still held" rather than "allocated at some
    // point".
    const BLOCKS: usize = 2048;
    let retained: Vec<Box<[u8; 4096]>> = (0..BLOCKS).map(|_| Box::new([0xA5_u8; 4096])).collect();
    std::hint::black_box(&retained);

    // 1. The allocations are sampled.
    let stats = mimalloc_pprof::prof::stats();
    assert!(
        stats.live_samples > 0,
        "{BLOCKS} retained 4 KiB blocks must appear in the live sample set \
         (live_bytes={}, enabled={})",
        stats.live_bytes,
        mimalloc_pprof::prof::is_enabled()
    );
    assert!(
        fbuild_daemon::heap_profile::live_sample_count() > 0,
        "the daemon helper must surface the same live sample count"
    );

    // 2. It serializes to pprof in memory — the shape a viewer consumes.
    let snapshot = mimalloc_pprof::prof::dump_proto_to_vec();
    assert!(
        !snapshot.is_empty(),
        "pprof profile.proto snapshot must not be empty"
    );

    // 3. And it can be written to a file from a *running* process. This is
    //    the half #1360 actually needed: the daemon was already wedged, and
    //    restarting it to enable a profiler would have destroyed the leak
    //    being investigated.
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let target = tmp.path().join("heap.pb");
    let written = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(fbuild_daemon::heap_profile::dump(Some(&target)))
        .expect("dump must succeed");
    assert_eq!(written.as_path(), target.as_path());
    let bytes = std::fs::read(written.as_path()).expect("dump file must be readable");
    assert!(!bytes.is_empty(), "dump file must not be empty");

    std::hint::black_box(&retained);
}
