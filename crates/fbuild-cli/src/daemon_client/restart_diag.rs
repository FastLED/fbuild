//! Evidence for the same-version daemon restart decision (FastLED/fbuild#1476).
//!
//! The direct acquisition path restarts a running daemon when this CLI's
//! sibling `fbuild-daemon` is newer on disk than the image the daemon reports.
//! In some CI jobs that fired on *every* command (~200 ms each) and the only
//! trace was a bare `daemon binary updated, restarting...` line, which cannot
//! tell apart a genuine rebuild, an mtime quirk, and a second launcher serving
//! the port. These helpers put both sides of the comparison on stderr.

use super::{HealthResponseFull, should_restart_daemon};

/// The `fbuild-daemon` binary next to this CLI, as used by the restart check.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct SiblingDaemon {
    pub path: Option<String>,
    /// Seconds since the Unix epoch; 0.0 when no sibling could be stat'ed.
    pub mtime: f64,
}

impl SiblingDaemon {
    pub fn discover() -> Self {
        let candidates =
            fbuild_core::platform::executable::current_image_sibling_candidates("fbuild-daemon");
        for candidate in candidates.into_iter().flatten() {
            let mtime = candidate
                .metadata()
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|mtime| mtime.duration_since(std::time::UNIX_EPOCH).ok());
            if let Some(mtime) = mtime {
                return Self {
                    path: Some(candidate.display().to_string()),
                    mtime: mtime.as_secs_f64(),
                };
            }
        }
        Self {
            path: None,
            mtime: 0.0,
        }
    }
}

/// `pid 42 v2.5.28 uptime 0.100s mtime 1790000000.5 exe /x/fbuild-daemon (...)`.
///
/// Uptime matters: a daemon that started milliseconds ago from the same path
/// yet reports an older mtime means the file changed after it started, not
/// that a stale daemon was left running.
fn describe_daemon(health: &HealthResponseFull) -> String {
    let launcher = match health.launched_by_broker {
        Some(true) => " (launched by running-process broker)",
        Some(false) => " (launched directly)",
        None => "",
    };
    format!(
        "pid {} v{} uptime {:.3}s mtime {} exe {}{}",
        health.pid,
        health.version,
        health.uptime_seconds,
        health.source_mtime,
        health.source_exe.as_deref().unwrap_or("<not reported>"),
        launcher
    )
}

fn describe_sibling(cli_version: &str, sibling: &SiblingDaemon) -> String {
    format!(
        "v{} sibling {} mtime {}",
        cli_version,
        sibling.path.as_deref().unwrap_or("<none>"),
        sibling.mtime
    )
}

/// The stderr line printed when the CLI restarts a running daemon.
///
/// Keeps the historical `daemon binary updated, restarting...` prefix so
/// existing log scrapers (the Blink benchmark) still match.
pub(super) fn restart_notice(
    health: &HealthResponseFull,
    cli_version: &str,
    sibling: &SiblingDaemon,
    acquisition: &str,
) -> String {
    let reason = if cli_version == health.version {
        format!(
            "sibling binary is {:.6}s newer",
            sibling.mtime - health.source_mtime
        )
    } else {
        "CLI version differs".to_string()
    };
    format!(
        "daemon binary updated, restarting... ({reason}; running daemon: {}; this CLI: {}; acquisition: {acquisition}; FastLED/fbuild#1476)",
        describe_daemon(health),
        describe_sibling(cli_version, sibling),
    )
}

/// After respawning, the daemon that answers `/health` must be one this CLI
/// would not restart again. If it still would, something other than this
/// CLI's spawn owns the endpoint (e.g. a daemon launched by the
/// running-process broker from a different image) and every later command
/// will pay for another restart. Returns the warning to print, if any.
pub(super) fn post_respawn_warning(
    health: &HealthResponseFull,
    cli_version: &str,
    sibling: &SiblingDaemon,
    spawned_pid: Option<u32>,
) -> Option<String> {
    if !should_restart_daemon(
        cli_version,
        &health.version,
        sibling.mtime,
        health.source_mtime,
    ) {
        return None;
    }
    let spawned = spawned_pid.map_or_else(|| "unknown".to_string(), |pid| pid.to_string());
    Some(format!(
        "warning: the daemon answering after the restart would be restarted again \
         (running daemon: {}; this CLI spawned pid {spawned}; this CLI: {}). \
         Another launcher is serving this endpoint, so every command will restart it \
         (FastLED/fbuild#1476).",
        describe_daemon(health),
        describe_sibling(cli_version, sibling),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn health(version: &str, mtime: f64) -> HealthResponseFull {
        HealthResponseFull {
            status: "healthy".to_string(),
            uptime_seconds: 1.0,
            version: version.to_string(),
            pid: 4242,
            source_mtime: mtime,
            source_exe: Some("/runner/target/release/fbuild-daemon".to_string()),
            launched_by_broker: Some(true),
        }
    }

    fn sibling(mtime: f64) -> SiblingDaemon {
        SiblingDaemon {
            path: Some("/runner/target/release/fbuild-daemon".to_string()),
            mtime,
        }
    }

    #[test]
    fn restart_notice_keeps_legacy_prefix_and_names_both_sides() {
        let notice = restart_notice(
            &health("2.5.28", 100.0),
            "2.5.28",
            &sibling(100.25),
            "direct daemon fallback",
        );
        assert!(notice.starts_with("daemon binary updated, restarting..."));
        assert!(notice.contains("sibling binary is 0.250000s newer"));
        assert!(notice.contains("pid 4242 v2.5.28 uptime 1.000s mtime 100 exe /runner/target/release/fbuild-daemon (launched by running-process broker)"));
        assert!(notice.contains(
            "this CLI: v2.5.28 sibling /runner/target/release/fbuild-daemon mtime 100.25"
        ));
        assert!(notice.contains("acquisition: direct daemon fallback"));
    }

    #[test]
    fn restart_notice_reports_version_upgrades() {
        let notice = restart_notice(&health("2.5.27", 200.0), "2.5.28", &sibling(100.0), "x");
        assert!(notice.contains("(CLI version differs;"));
    }

    #[test]
    fn restart_notice_tolerates_daemons_without_identity_fields() {
        let mut old = health("2.5.28", 1.0);
        old.source_exe = None;
        old.launched_by_broker = None;
        let notice = restart_notice(&old, "2.5.28", &sibling(2.0), "x");
        assert!(notice.contains("exe <not reported>;"));
    }

    #[test]
    fn no_post_respawn_warning_when_the_new_daemon_matches_the_sibling() {
        assert_eq!(
            post_respawn_warning(&health("2.5.28", 100.0), "2.5.28", &sibling(100.0), Some(7)),
            None
        );
    }

    #[test]
    fn post_respawn_warning_flags_an_older_image_still_serving() {
        let warning =
            post_respawn_warning(&health("2.5.28", 100.0), "2.5.28", &sibling(150.0), Some(7))
                .expect("an older image after respawn must be flagged");
        assert!(warning.contains("this CLI spawned pid 7"));
        assert!(warning.contains("pid 4242 v2.5.28 uptime 1.000s mtime 100"));
        assert!(warning.contains("FastLED/fbuild#1476"));
    }
}
