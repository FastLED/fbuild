//! Curated user-facing CLI output API.
//!
//! FastLED/fbuild#844 ("Bridge pair 12"). All user-facing output from
//! fbuild flows through one of these six functions. The matching
//! `ban_print_in_production` dylint forbids `println!` /  `eprintln!`
//! in `crates/fbuild-cli/src/` and `crates/fbuild-build/src/` (this
//! file is the sole exemption — it IS the bridge).
//!
//! The split is intentional:
//!
//! | Fn           | Sink              | Use for                          |
//! |--------------|-------------------|----------------------------------|
//! | `progress`   | `tracing::info!`  | "Building env X…", spinner-ish   |
//! | `result`     | `println!`        | The final answer — what stdout-pipers see |
//! | `diagnostic` | `eprintln!`       | Final operation stderr returned by the daemon |
//! | `warn`       | `tracing::warn!`  | Non-fatal recoverables           |
//! | `error`      | `tracing::error!` | Fatal errors                     |
//! | `debug`      | `tracing::debug!` | `--verbose` / `RUST_LOG=debug`   |
//!
//! Progress follows tracing filters. Final results, daemon diagnostics, and
//! actionable warnings use stdout/stderr directly so operation output survives
//! redirection and filtering. Visible warnings use yellow only on a terminal
//! without `NO_COLOR`; JSON stdout remains unchanged.

use std::fmt::Display;
use std::io::{IsTerminal, Write};
use std::sync::{Mutex, OnceLock};

fn render_warning(message: &str, color: bool) -> String {
    let message = if message.starts_with("WARNING:") {
        message.to_string()
    } else {
        format!("WARNING: {message}")
    };
    if color {
        format!("\x1b[33m{message}\x1b[0m")
    } else {
        message
    }
}

/// Actionable warnings must survive tracing filters and never pollute JSON stdout.
pub fn visible_warning(message: impl Display) {
    static SHOWN: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    let message = render_warning(&message.to_string(), false);
    if let Ok(mut shown) = SHOWN.get_or_init(Mutex::default).lock() {
        if !shown.insert(message.clone()) {
            return;
        }
    }
    let color = std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    eprintln!("{}", render_warning(&message, color));
}

/// Explicit consent prompt on stderr; callers establish interactivity first.
pub fn confirmation_prompt(message: &str) -> std::io::Result<bool> {
    let mut stderr = std::io::stderr().lock();
    write!(stderr, "{message} [y/N] ")?;
    stderr.flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(crate::cli::windows_long_paths::consented(&answer))
}

/// In-progress narration. Emits at `INFO` so a default-verbosity user
/// sees it; `--quiet` hides it. Examples: "Building env esp32dev…",
/// "Downloaded xtensa-esp32-elf-12.2.0".
pub fn progress(msg: impl Display) {
    tracing::info!("{msg}");
}

/// The final answer. The only output that must survive `--quiet` and
/// pipe redirection (e.g. `fbuild show fqbn esp32dev | xargs ...`).
/// Use sparingly — most output is `progress`, not `result`.
pub fn result(msg: impl Display) {
    let message = msg.to_string();
    if message.lines().any(|line| line.starts_with("WARNING:")) {
        for line in message.split('\n') {
            if line.starts_with("WARNING:") {
                visible_warning(line);
            } else {
                println!("{line}");
            }
        }
    } else {
        println!("{message}");
    }
}

/// Final operation diagnostics returned by the daemon. Unlike transient
/// tracing warnings, these must remain visible with the default filter,
/// `--quiet`, and when stderr is redirected by automation.
pub fn diagnostic(msg: impl Display) {
    for line in msg.to_string().lines() {
        if line.starts_with("WARNING:") {
            visible_warning(line);
        } else {
            eprintln!("{line}");
        }
    }
}

/// Non-fatal warning. Emits at `WARN`. Use for situations that don't
/// stop the operation but the user should know about — deprecated
/// flags, missing-but-not-required files, etc.
pub fn warn(msg: impl Display) {
    tracing::warn!("{msg}");
}

/// Fatal error. Emits at `ERROR`. The caller is expected to also
/// surface a non-zero exit code; this function does NOT exit.
pub fn error(msg: impl Display) {
    tracing::error!("{msg}");
}

/// Debug output. Emits at `DEBUG`. Hidden by default; surfaces with
/// `--verbose` or `RUST_LOG=debug`.
pub fn debug(msg: impl Display) {
    tracing::debug!("{msg}");
}

#[cfg(test)]
mod warning_tests {
    use super::*;

    #[test]
    fn warning_is_yellow_only_when_color_is_allowed() {
        let yellow = render_warning("Using short-path fallback", true);
        assert!(yellow.contains("\x1b[33m"));
        assert!(yellow.contains("WARNING: Using short-path fallback"));
        let plain = render_warning("Using short-path fallback", false);
        assert_eq!(plain, "WARNING: Using short-path fallback");
        assert!(!plain.contains('\x1b'));
    }

    #[test]
    fn warning_prefix_is_not_duplicated() {
        assert_eq!(
            render_warning("WARNING: existing", false),
            "WARNING: existing"
        );
    }
}
