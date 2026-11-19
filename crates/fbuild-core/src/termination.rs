//! Naming the signal that killed a subprocess (FastLED/fbuild#1628, #1630).
//!
//! A compiler killed by a signal does not exit — it is *terminated*. `wait`
//! reports that as `-signal` on Unix ([`crate::platform::linux::process`] and
//! its macOS sibling pass `-status.signal()` through), so the only thing an
//! error message carried was a bare `exit code -9` next to empty stderr. That
//! reads like a compiler bug or a source error when it is neither: something on
//! the host sent SIGKILL, and the process had no chance to print anything.
//!
//! The daemon's containment path, the OOM killer and a dying owner process all
//! kill exactly this way, so a kill must say so — signal name, the fact that it
//! was not an exit status, and where to look next ([`KILL_HINT`]).
//!
//! Two exit-code conventions reach us and both are recognised:
//! - `-signal`, what [`crate::subprocess::ToolOutput`] carries on Unix;
//! - `128 + signal`, what a shell or a tool that shells out reports (137 for
//!   SIGKILL), which is what zccache forwards for a killed child.
//!
//! Windows reports `-1` for any abnormal exit (`status.code().unwrap_or(-1)`)
//! and keeps no signal number, so [`termination`] answers `None` there: naming
//! a signal would be a guess.

use crate::platform::host;

/// Signals whose numbers are identical on every Unix fbuild runs on.
///
/// The full table is not portable — Linux and macOS disagree on SIGBUS (7/10),
/// SIGUSR1 (10/30), SIGUSR2 (12/31), SIGCONT (18/19) and SIGSTKFLT. Naming one
/// of those by number alone would print the wrong name, so an unlisted signal
/// is described by number only.
const PORTABLE_SIGNALS: &[(i32, &str)] = &[
    (1, "SIGHUP"),
    (2, "SIGINT"),
    (3, "SIGQUIT"),
    (4, "SIGILL"),
    (5, "SIGTRAP"),
    (6, "SIGABRT"),
    (8, "SIGFPE"),
    (9, "SIGKILL"),
    (11, "SIGSEGV"),
    (13, "SIGPIPE"),
    (14, "SIGALRM"),
    (15, "SIGTERM"),
    (24, "SIGXCPU"),
    (25, "SIGXFSZ"),
];

/// The closing line of every signal-kill message. Shared so a library compile,
/// a link step and (once they report through it) the other subprocess paths
/// cannot drift into three different explanations of the same event.
pub const KILL_HINT: &str = "the process was killed by a signal, not exited — a host or \
containment event (OOM killer, the fbuild daemon's containment, or a dead parent process), \
not a source or compiler error; retry once, then check the daemon log and `dmesg` for kills";

/// A subprocess that ended without an exit status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Termination {
    /// The Unix signal number that ended the process.
    pub signal: i32,
    /// True when the code was `128 + signal` rather than `-signal`.
    pub via_shell_code: bool,
}

impl Termination {
    /// The signal's conventional name, or `None` when it is not portable.
    pub fn name(self) -> Option<&'static str> {
        PORTABLE_SIGNALS
            .iter()
            .find(|(number, _)| *number == self.signal)
            .map(|(_, name)| *name)
    }

    /// Short parenthetical label, e.g. `SIGKILL (signal 9)`.
    pub fn label(self) -> String {
        match self.name() {
            Some(name) => format!("{name} (signal {})", self.signal),
            None => format!("signal {} (no portable name)", self.signal),
        }
    }

    /// One line for a log, naming the kill and what it means.
    pub fn log_line(self) -> String {
        let convention = if self.via_shell_code {
            " by shell convention 128+signal"
        } else {
            ""
        };
        format!("was killed by {}{convention} — {KILL_HINT}", self.label())
    }
}

/// Interpret an exit code as a signal kill, when the host can know one.
///
/// `is_unix_host` is a parameter rather than a call to [`host::current`] so the
/// classification is testable on every host.
pub fn termination(exit_code: i32, is_unix_host: bool) -> Option<Termination> {
    if !is_unix_host {
        return None;
    }
    if exit_code < 0 {
        let signal = -exit_code;
        return (signal > 0).then_some(Termination {
            signal,
            via_shell_code: false,
        });
    }
    // 128+N is the shell convention. 129..=192 keeps the range to real signals
    // (1..=64) and never blanks out an ordinary small exit status.
    if (129..=192).contains(&exit_code) {
        return Some(Termination {
            signal: exit_code - 128,
            via_shell_code: true,
        });
    }
    None
}

/// [`termination`] for the machine running this process.
pub fn host_termination(exit_code: i32) -> Option<Termination> {
    termination(exit_code, host::current().is_unix())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negative_codes_name_the_signal() {
        let kill = termination(-9, true).expect("-9 is a signal kill");
        assert_eq!(kill.signal, 9);
        assert_eq!(kill.name(), Some("SIGKILL"));
        assert_eq!(kill.label(), "SIGKILL (signal 9)");
        assert!(!kill.via_shell_code);
        assert!(kill.log_line().contains("killed by SIGKILL (signal 9)"));
        assert!(kill.log_line().contains("not a source or compiler error"));
    }

    #[test]
    fn shell_convention_codes_are_recognised_and_labelled() {
        let kill = termination(137, true).expect("137 is SIGKILL by shell convention");
        assert_eq!(kill.signal, 9);
        assert!(kill.via_shell_code);
        assert!(kill.log_line().contains("shell convention 128+signal"));
        let term = termination(143, true).expect("143 is SIGTERM");
        assert_eq!(term.name(), Some("SIGTERM"));
    }

    #[test]
    fn an_ordinary_exit_status_is_not_a_kill() {
        for code in [0, 1, 2, 127, 128, 193, 255] {
            assert_eq!(termination(code, true), None, "code {code}");
            assert_eq!(termination(code, false), None, "code {code}");
        }
    }

    #[test]
    fn windows_reports_no_signal() {
        // `exit_code` maps every abnormal Windows exit to -1; naming SIGHUP
        // there would be a fabrication.
        assert_eq!(termination(-1, false), None);
    }

    /// The real thing, not a table lookup: a child that SIGKILLs itself
    /// reaches us as `-9` with empty stderr, which is the shape #1628 saw.
    #[tokio::test]
    async fn a_really_sigkilled_child_classifies_as_a_kill() {
        if !host::is_unix() {
            return;
        }
        let output = crate::subprocess::run_command(
            &["sh", "-c", "kill -9 $$"],
            None,
            None,
            Some(std::time::Duration::from_secs(30)),
        )
        .await
        .expect("sh spawns");
        assert_eq!(output.exit_code, -9, "stderr: {}", output.stderr);
        assert!(output.stderr.is_empty(), "{}", output.stderr);
        let termination = host_termination(output.exit_code).expect("classifies");
        assert_eq!(termination.name(), Some("SIGKILL"));
        assert_eq!(termination.label(), "SIGKILL (signal 9)");
    }

    #[test]
    fn unportable_signals_are_named_by_number_only() {
        let bus = termination(-10, true).expect("-10 is signal 10");
        assert_eq!(bus.name(), None, "SIGBUS is 7 on Linux, 10 on macOS");
        assert_eq!(bus.label(), "signal 10 (no portable name)");
    }
}
