//! Attribution of a daemon that vanished mid-operation to its own diagnostics.
//!
//! FastLED/fbuild#1866: the daemon embeds `zccache`, whose lock watchdogs call
//! `std::process::abort()` on an overdue *holder*. An abort is not unwindable —
//! it takes the whole daemon (and every unrelated build, deploy and monitor
//! request in flight) with it, and the client can only observe that the
//! connection died. The watchdog does, however, write a full holder/waiter
//! report to the daemon log immediately before terminating.
//!
//! This module turns that report into a typed verdict so a client sees *what*
//! killed the daemon instead of a bare `error decoding response body`: the
//! cache-lock watchdog class is a backend failure, not a user, device or
//! firmware fault, and the distinction decides whether a retry is meaningful.
//!
//! Parsing is deliberately shape-tolerant: the report is produced by an
//! upstream crate that may add fields, and `age_ns` values reach the log
//! through a redaction layer, so a token that is not a number is skipped
//! rather than trusted.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Duration;

/// Marker line the upstream watchdog prints before it aborts the process.
pub const WATCHDOG_SIGNATURE: &str = "lock hold exceeded its budget";

/// Prefix of every watchdog report block (`zccache: lock hold exceeded ...`).
const WATCHDOG_BLOCK_PREFIX: &str = "zccache: lock";

/// Bytes of the daemon log read when diagnosing. Enough to hold several
/// watchdog reports plus the surrounding build lines, small enough to read on
/// an interactive failure path.
pub const DEFAULT_TAIL_BYTES: u64 = 96 * 1024;

/// Whether a registered guard was being held or was queued behind one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LockPhase {
    /// The thread owned the guard past its hold budget — the abort trigger.
    Holding,
    /// The thread was queued behind a guard and never got it.
    Waiting,
}

/// One guard named by a watchdog report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockSite {
    pub phase: LockPhase,
    /// Guard shape as the backend spells it (`Mutex`, `RwLock(read)`, ...).
    pub kind: String,
    /// How long the guard had been held (or waited for) when it was reported.
    pub age: Duration,
    /// The backend's hold budget for this guard.
    pub budget: Duration,
    /// `file:line:column` of the acquisition site.
    pub site: String,
    pub thread: String,
}

impl LockSite {
    /// True when the reported age overran the budget it was measured against.
    pub fn overran(&self) -> bool {
        self.age > self.budget
    }
}

/// What the daemon's own log says about why it is gone.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DaemonLoss {
    /// The embedded cache lock watchdog aborted the process.
    WatchdogAbort {
        /// Overdue holders, longest-held first. Never empty for a parsed
        /// report, but an empty list is tolerated: the signature line alone is
        /// already evidence of the abort class.
        holders: Vec<LockSite>,
        waiters: usize,
    },
    /// The process died from a Rust panic (a `panic = "abort"` path).
    Panic { message: String },
    /// Nothing in the tail explains the exit.
    Unattributed,
}

impl DaemonLoss {
    /// One-line classification, for logs and tests.
    pub fn headline(&self) -> String {
        match self {
            Self::WatchdogAbort { holders, waiters } => match holders.first() {
                Some(holder) => format!(
                    "the embedded zccache lock watchdog aborted the daemon: holding a {} for \
                     {:.2}s exceeded its {:.2}s budget at {} (thread {}), {waiters} waiter(s) \
                     queued{}",
                    holder.kind,
                    holder.age.as_secs_f64(),
                    holder.budget.as_secs_f64(),
                    holder.site,
                    holder.thread,
                    if holders.len() > 1 {
                        format!("; {} further holder(s) reported", holders.len() - 1)
                    } else {
                        String::new()
                    }
                ),
                None => format!(
                    "the embedded zccache lock watchdog aborted the daemon: a lock hold exceeded \
                     its budget, {waiters} waiter(s) queued"
                ),
            },
            Self::Panic { message } => format!("the daemon died from a panic: {message}"),
            Self::Unattributed => {
                "the daemon process is gone and its log carries no abort diagnostics".to_string()
            }
        }
    }

    /// Full client-facing explanation. `log_path` is echoed so the user can
    /// reach the complete holder report.
    pub fn explain(&self, log_path: &Path) -> String {
        let mut text = self.headline();
        if let Self::WatchdogAbort { .. } = self {
            text.push_str(
                ". That is the embedded cache-watchdog failure class \
                 (FastLED/fbuild#1866): an infrastructure fault inside the daemon process, not a \
                 device, firmware, upload or build error. The in-flight operation was interrupted \
                 and must be retried once a daemon is serving again",
            );
        }
        text.push_str(&format!("; full diagnostics: {}", log_path.display()));
        text
    }
}

/// Read at most `max_bytes` from the end of a log file.
///
/// A tail that starts mid-record or mid-codepoint is fine: every consumer here
/// scans whole lines and ignores what it cannot parse.
pub fn read_log_tail(path: &Path, max_bytes: u64) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(max_bytes);
    file.seek(SeekFrom::Start(start))?;
    let mut buffer = Vec::new();
    file.take(max_bytes).read_to_end(&mut buffer)?;
    Ok(String::from_utf8_lossy(&buffer).into_owned())
}

/// Classify a daemon log tail. The *last* report block wins: a daemon that
/// logged an earlier overrun and kept building died of whatever it logged
/// last.
pub fn diagnose(log_tail: &str) -> DaemonLoss {
    if let Some(block) = last_watchdog_block(log_tail) {
        return watchdog_verdict(block);
    }
    if let Some(message) = last_panic_message(log_tail) {
        return DaemonLoss::Panic { message };
    }
    DaemonLoss::Unattributed
}

fn last_watchdog_block(tail: &str) -> Option<&str> {
    let signature = tail.rfind(WATCHDOG_SIGNATURE)?;
    let start = tail[..signature]
        .rfind(WATCHDOG_BLOCK_PREFIX)
        .unwrap_or(signature);
    Some(&tail[start..])
}

fn watchdog_verdict(block: &str) -> DaemonLoss {
    let mut holders = Vec::new();
    let mut waiters = 0usize;
    for line in block.lines() {
        let Some(site) = parse_lock_site(line) else {
            continue;
        };
        if site.phase == LockPhase::Holding {
            holders.push(site);
        } else {
            waiters += 1;
        }
    }
    holders.sort_by_key(|site| std::cmp::Reverse(site.age));
    DaemonLoss::WatchdogAbort { holders, waiters }
}

fn last_panic_message(tail: &str) -> Option<String> {
    let mut message = None;
    for line in tail.lines() {
        if let Some(at) = line.find("panicked at ") {
            message = Some(line[at + "panicked at ".len()..].trim().to_string());
        }
    }
    let message = message?;
    (!message.is_empty()).then_some(message)
}

/// Parse one `Holding`/`Waiting` report line.
///
/// Accepted shape (fields beyond the first three are optional):
/// `Holding Mutex ... age_ns=<n> budget_ns=<n> at <site> (thread <name>)`
fn parse_lock_site(line: &str) -> Option<LockSite> {
    let (phase, rest) = line.trim().split_once(' ')?;
    let phase = match phase {
        "Holding" => LockPhase::Holding,
        "Waiting" => LockPhase::Waiting,
        _ => return None,
    };
    let kind = rest.split_whitespace().next()?.trim().to_string();
    let age = nanos_after(line, "age_ns=")?;
    let budget = nanos_after(line, "budget_ns=")?;
    let raw_site = between(line, " at ", " (thread ").or_else(|| after(line, " at "))?;
    let thread = between(line, "(thread ", ")").unwrap_or_default();
    Some(LockSite {
        phase,
        kind,
        age: Duration::from_nanos(age),
        budget: Duration::from_nanos(budget),
        site: shorten_site(raw_site.trim()),
        thread: thread.trim().to_string(),
    })
}

/// Value of `key` as `u64`, taking only the leading decimal digits so a
/// trailing `)` or a redacted placeholder does not have to be special-cased.
fn nanos_after(line: &str, key: &str) -> Option<u64> {
    let rest = after(line, key)?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

fn after<'a>(haystack: &'a str, key: &str) -> Option<&'a str> {
    let at = haystack.find(key)?;
    Some(&haystack[at + key.len()..])
}

fn between<'a>(haystack: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let rest = after(haystack, open)?;
    let end = rest.find(close)?;
    Some(&rest[..end])
}

/// Keep the last two path components of an acquisition site so the message
/// stays readable (`.../zccache-1.15.8/src/artifact/store.rs:492:14` ->
/// `artifact/store.rs:492:14`).
fn shorten_site(site: &str) -> String {
    let (path, suffix) = match site.rsplit_once(':') {
        Some((path, last)) => match path.rsplit_once(':') {
            Some((path, line)) => (path, format!(":{line}:{last}")),
            None => (path, format!(":{last}")),
        },
        None => (site, String::new()),
    };
    let mut parts = path.rsplit('/');
    let tail: Vec<&str> = parts.by_ref().take(2).collect();
    let shortened = if tail.len() == 2 {
        format!("{}/{}", tail[1], tail[0])
    } else {
        path.to_string()
    };
    if shortened.is_empty() {
        site.to_string()
    } else {
        format!("{shortened}{suffix}")
    }
}
