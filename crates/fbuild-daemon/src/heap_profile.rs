//! Sampled heap profiling for the long-lived daemon (FastLED/fbuild#1361).
//!
//! # Why this exists
//!
//! `fbuild-daemon` is the one fbuild process that outlives a build. It holds
//! caches, device state, and broadcast buffers, so it is where a slow leak is
//! both most likely and least visible — FastLED/fbuild#1360 caught it at
//! ~3.9 GB of resident memory while *idle*, still climbing, until the health
//! probe failed and every build started reporting a compile error.
//!
//! The allocator underneath is `mimalloc-pprof`: the same mimalloc the daemon
//! already used, with a sampled heap profiler attached. That profiler is
//! dormant until something starts it, which is what makes it safe to ship
//! unconditionally rather than behind a build feature. A profiler that has to
//! be compiled in specially is never compiled in on the machine where the leak
//! actually reproduces — and #1360 was found on a real Windows box, twice in
//! one session, with no minimal reproduction.
//!
//! # Two ways in, because leaks are found at two different times
//!
//! - [`start_from_env`] reads `FBUILD_HEAP_PROFILE` at startup. Startup and
//!   static-initialization allocations are only visible this way, because
//!   anything allocated before the profiler starts is untracked.
//! - [`dump`] can be called on a daemon that is *already* wedged, over HTTP,
//!   without restarting it. This matters more than it looks: restarting the
//!   daemon destroys the leak, so a restart-only profiler cannot answer the
//!   question it exists to answer.
//!
//! Snapshots are pprof `profile.proto` — the same format `go tool pprof` and
//! every flame-graph viewer already read, so nothing here needs a bespoke
//! decoder.

use std::path::{Path, PathBuf};

/// Environment variable that turns heap profiling on at daemon startup.
///
/// Set to `1` (or any value other than `0`/empty) before the daemon starts.
/// The value may also be a decimal sample rate in bytes, e.g.
/// `FBUILD_HEAP_PROFILE=65536` to sample roughly every 64 KiB allocated.
pub const HEAP_PROFILE_ENV: &str = "FBUILD_HEAP_PROFILE";

/// Sample rate used when [`HEAP_PROFILE_ENV`] is set to a plain truthy value.
///
/// 512 KiB is `mimalloc-pprof`'s own default. Sampling is statistical, so a
/// finer rate buys resolution with proportional overhead; a leak large enough
/// to wedge a daemon is visible at this rate.
const DEFAULT_SAMPLE_RATE: usize = 512 * 1024;

/// Directory under the dev/prod-isolated fbuild root where dumps are written.
const DUMP_SUBDIR: &str = "heap-profiles";

/// Start the profiler if [`HEAP_PROFILE_ENV`] asks for it.
///
/// Returns the sample rate actually used, or `None` when profiling was not
/// requested or the profiler was already running. Call this as early in
/// `main` as possible — allocations made before it are invisible to every
/// later snapshot.
pub fn start_from_env() -> Option<usize> {
    let requested = std::env::var(HEAP_PROFILE_ENV).ok()?;
    let rate = sample_rate_from(&requested)?;
    if mimalloc_pprof::prof::start(rate) {
        Some(rate)
    } else {
        None
    }
}

/// Parse the env var into a sample rate.
///
/// Separated from [`start_from_env`] so the parsing contract is testable
/// without touching process-wide profiler state.
fn sample_rate_from(value: &str) -> Option<usize> {
    let value = value.trim();
    match value {
        "" | "0" | "false" | "off" => None,
        "1" | "true" | "on" => Some(DEFAULT_SAMPLE_RATE),
        other => other.parse::<usize>().ok().filter(|rate| *rate > 0),
    }
}

/// Whether the profiler is currently sampling.
pub fn is_enabled() -> bool {
    mimalloc_pprof::prof::is_enabled()
}

/// Start profiling at `sample_rate` bytes, for a daemon that is already up.
///
/// Returns `false` when a session was already running — its rate stays in
/// effect rather than being silently replaced.
pub fn start(sample_rate: usize) -> bool {
    mimalloc_pprof::prof::start(sample_rate)
}

/// Stop sampling. Retained samples stay dumpable.
pub fn stop() {
    mimalloc_pprof::prof::stop();
}

/// Where [`dump`] writes, absent an explicit path.
///
/// Routed through `fbuild_paths` so dumps land under the same dev/prod
/// isolation as everything else the daemon writes, rather than the CWD of
/// whoever happened to start it.
pub fn default_dump_dir() -> PathBuf {
    fbuild_paths::temp_subdir(DUMP_SUBDIR)
}

/// Write a pprof `profile.proto` snapshot of the live heap.
///
/// `path` overrides the destination; otherwise the snapshot lands in
/// [`default_dump_dir`] under a name carrying the daemon's PID, so repeated
/// dumps from one daemon do not overwrite each other and dumps from
/// different daemons do not collide.
///
/// Returns the path written. Dumping while the profiler is stopped produces
/// a valid but empty profile, so the caller gets a file either way and the
/// emptiness is visible in the profile itself rather than as an error.
pub fn dump(path: Option<&Path>) -> std::io::Result<PathBuf> {
    let target = match path {
        Some(explicit) => explicit.to_path_buf(),
        None => {
            let dir = default_dump_dir();
            std::fs::create_dir_all(&dir)?;
            dir.join(format!("heap-{}.pb", std::process::id()))
        }
    };
    mimalloc_pprof::prof::dump_proto_file(&target)?;
    Ok(target)
}

/// Live-sample counters, for a health endpoint that wants to report growth
/// without serializing a whole profile.
///
/// `live_samples` is the sampled count; `live_bytes` the sampled byte total.
/// Both are statistical — the exact allocator counters they are derived from
/// are what make an assertion on a sampled profile meaningful.
pub fn live_sample_count() -> usize {
    mimalloc_pprof::prof::stats().live_samples
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_values_that_mean_off_do_not_start_a_profiler() {
        for value in ["", "0", "false", "off", "   "] {
            assert_eq!(
                sample_rate_from(value),
                None,
                "{value:?} must not enable profiling"
            );
        }
    }

    #[test]
    fn truthy_env_values_select_the_default_rate() {
        for value in ["1", "true", "on", " on "] {
            assert_eq!(
                sample_rate_from(value),
                Some(DEFAULT_SAMPLE_RATE),
                "{value:?} must select the default rate"
            );
        }
    }

    #[test]
    fn a_numeric_env_value_is_taken_as_the_sample_rate() {
        assert_eq!(sample_rate_from("65536"), Some(65536));
        // A rate of zero would mean "sample everything", which is not what an
        // operator typing 0 means — they mean off, and that is the `"0"` case.
        assert_eq!(sample_rate_from("00"), None);
        assert_eq!(sample_rate_from("not-a-number"), None);
    }

    #[test]
    fn the_default_dump_dir_is_under_the_isolated_fbuild_root() {
        let dir = default_dump_dir();
        assert!(
            dir.ends_with(DUMP_SUBDIR),
            "dump dir must be the named subdir, got {}",
            dir.display()
        );
        assert!(
            dir.starts_with(fbuild_paths::get_fbuild_root()),
            "dump dir must sit under the dev/prod-isolated root, got {}",
            dir.display()
        );
    }
}
