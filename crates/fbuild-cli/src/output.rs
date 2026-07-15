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
//! Everything except `result` and `diagnostic` is routed through `tracing` so
//! `--color={auto,always,never}`, `--quiet`, and `--verbose` flow through the
//! level filter in one place. Final results and daemon diagnostics use stdout
//! and stderr directly so actionable operation output survives redirection,
//! piping, and `--quiet`.

use std::fmt::Display;

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
    println!("{msg}");
}

/// Final operation diagnostics returned by the daemon. Unlike transient
/// tracing warnings, these must remain visible with the default filter,
/// `--quiet`, and when stderr is redirected by automation.
pub fn diagnostic(msg: impl Display) {
    eprintln!("{msg}");
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
