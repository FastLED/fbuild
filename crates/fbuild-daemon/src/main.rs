//! The `fbuild-daemon` binary.
//!
//! FastLED/fbuild#1851 phase 1 keeps this executable (external callers, service
//! definitions and the running-process broker still exec it by name), but the
//! implementation lives in the library at [`fbuild_daemon::entry`]. The
//! `#[global_allocator]` is deliberately still declared here: it is a bin-level
//! item and must stay with the executable that ships the heap profiler.

// The same mimalloc the daemon has always used, plus a sampled heap
// profiler that stays dormant until started (FastLED/fbuild#1361).
// Unconditional rather than feature-gated: a profiler compiled out of
// the shipped binary is never present on the machine where a slow leak
// reproduces, which is exactly how #1360 was found.
#[global_allocator]
static GLOBAL: mimalloc_pprof::MiMalloc = mimalloc_pprof::MiMalloc;

fn main() {
    // FastLED/fbuild#1742: this binary's commit stamp, installed before the
    // daemon reads it for `/health` and broker registration.
    let _ = fbuild_core::build_id::STAMP.set(fbuild_core::build_stamp!());
    std::process::exit(fbuild_daemon::entry::run());
}
