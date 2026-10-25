//! Why a spawned daemon never became healthy, read back from its own log.
//!
//! The daemon records the startup failure (cache-root ownership busy, a bind
//! collision, a panic) in `daemon.log`, but the CLI used to report only "did
//! not become healthy after 3 spawn attempts", so a CI flake left nothing to
//! diagnose (FastLED/fbuild#1627). This module pulls the warnings and errors
//! the daemon logged during this CLI's spawn attempts into that error.

/// How many problem lines to quote; enough for one line per spawn attempt
/// plus a retry warning or two, without burying the error.
const MAX_LINES: usize = 8;

/// UTC RFC 3339 timestamp with microseconds, the format `tracing` prefixes
/// each `daemon.log` line with, so log timestamps compare as strings.
pub(super) fn log_timestamp(time: std::time::SystemTime) -> String {
    let micros = time
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros();
    let secs = (micros / 1_000_000) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil-from-days (Howard Hinnant), valid for the proleptic Gregorian calendar.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:06}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        micros % 1_000_000
    )
}

/// WARN/ERROR lines logged at or after `since` (a [`log_timestamp`]), with
/// ANSI colour stripped, most recent last.
pub(super) fn startup_problems(log: &str, since: &str) -> Vec<String> {
    let mut lines: Vec<String> = log
        .lines()
        .map(strip_ansi)
        .filter(|line| line.get(..since.len()).is_some_and(|stamp| stamp >= since))
        .filter(|line| line.contains(" WARN ") || line.contains(" ERROR "))
        .collect();
    let excess = lines.len().saturating_sub(MAX_LINES);
    lines.drain(..excess);
    lines
}

/// The block appended to the "did not become healthy" error, or empty when
/// the daemon logged nothing useful.
pub(super) fn startup_problems_note(since: &str) -> String {
    let path = fbuild_paths::get_daemon_log_file();
    let Ok(log) = std::fs::read_to_string(&path) else {
        return String::new();
    };
    let problems = startup_problems(&log, since);
    if problems.is_empty() {
        return String::new();
    }
    format!(
        "\n\nThe daemon logged these problems during the spawn attempts ({}):\n  {}",
        path.display(),
        problems.join("\n  ")
    )
}

fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // CSI: ESC '[' params... final byte in '@'..='~'.
            if chars.next() == Some('[') {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Wall-clock budget for a spawned daemon to answer `health()`. Stretched while
/// the daemon reports `starting` (FastLED/fbuild#1480).
///
/// Loaded CI runners need longer than a dev box (#1627), so the default is 30 s
/// under `CI` and 10 s otherwise; `FBUILD_DAEMON_READINESS_TIMEOUT_SECS`
/// overrides both.
pub(super) fn readiness_timeout_secs() -> u64 {
    readiness_timeout_from(
        std::env::var("FBUILD_DAEMON_READINESS_TIMEOUT_SECS").ok().as_deref(),
        std::env::var_os("CI").is_some(),
    )
}

fn readiness_timeout_from(override_secs: Option<&str>, is_ci: bool) -> u64 {
    match override_secs.and_then(|v| v.trim().parse::<u64>().ok()) {
        Some(secs) if secs > 0 => secs,
        _ if is_ci => 30,
        _ => 10,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn readiness_timeout_defaults_and_override() {
        use super::readiness_timeout_from as t;
        assert_eq!(t(None, false), 10);
        assert_eq!(t(None, true), 30);
        assert_eq!(t(Some("45"), false), 45);
        assert_eq!(t(Some("0"), true), 30);
        assert_eq!(t(Some("junk"), false), 10);
    }

    use super::*;

    #[test]
    fn timestamp_matches_tracing_format() {
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_micros(1_792_903_040_071_970);
        assert_eq!(log_timestamp(t), "2026-10-25T04:37:20.071970Z");
        assert_eq!(
            log_timestamp(std::time::UNIX_EPOCH),
            "1970-01-01T00:00:00.000000Z"
        );
    }

    #[test]
    fn keeps_only_recent_warnings_and_errors_without_colour() {
        let log = "\
\u{1b}[2m2026-10-25T04:00:00.000000Z\u{1b}[0m \u{1b}[33m WARN\u{1b}[0m old run: bind failed\n\
\u{1b}[2m2026-10-25T05:00:00.500000Z\u{1b}[0m \u{1b}[32m INFO\u{1b}[0m fbuild daemon starting on port 11393\n\
\u{1b}[2m2026-10-25T05:00:01.000000Z\u{1b}[0m \u{1b}[33m WARN\u{1b}[0m bind to 127.0.0.1:11393 failed (Address in use)\n\
\u{1b}[2m2026-10-25T05:00:02.000000Z\u{1b}[0m \u{1b}[31mERROR\u{1b}[0m another fbuild-daemon owns the cache root\n\
not a log line\n";
        let problems = startup_problems(log, "2026-10-25T05:00:00.000000Z");
        assert_eq!(
            problems,
            [
                "2026-10-25T05:00:01.000000Z  WARN bind to 127.0.0.1:11393 failed (Address in use)",
                "2026-10-25T05:00:02.000000Z ERROR another fbuild-daemon owns the cache root",
            ]
        );
    }

    #[test]
    fn quotes_at_most_the_last_few_lines() {
        let log: String = (0..20)
            .map(|i| format!("2026-10-25T05:00:{i:02}.000000Z  WARN attempt {i}\n"))
            .collect();
        let problems = startup_problems(&log, "2026-10-25T05:00:00.000000Z");
        assert_eq!(problems.len(), MAX_LINES);
        assert!(problems.last().unwrap().ends_with("attempt 19"));
    }
}
