//! `fbuild sync` CLI adapter — parses argv, calls into
//! [`crate::sync::run_sync`], propagates exit code.
//!
//! FastLED/fbuild#618 Phase 1. The heavy lifting lives in
//! `crates/fbuild-cli/src/sync/`; this file is intentionally thin so the
//! CLI wiring is easy to audit.

use std::path::PathBuf;

use crate::sync::{run_sync, SyncArgs, SyncOutcome};

/// Adapter for `Commands::Sync` — invoked from `cli::dispatch`.
pub async fn run_sync_cmd(
    project_dir: Option<PathBuf>,
    environment: Option<String>,
    yes: bool,
    locked: bool,
    check: bool,
    dry_run: bool,
    upgrade: bool,
    upgrade_package: Option<String>,
) -> i32 {
    let args = SyncArgs {
        project_dir,
        environment,
        yes,
        locked,
        check,
        dry_run,
        upgrade,
        upgrade_package,
    };
    let outcome = run_sync(args).await;
    // Log the non-Wrote/NoOp/DryRun outcomes on stderr so failure modes
    // surface without needing --verbose.
    match &outcome {
        SyncOutcome::Wrote(_) | SyncOutcome::NoOp | SyncOutcome::DryRun => {}
        SyncOutcome::CheckPassed => eprintln!("fbuild sync --check: OK"),
        SyncOutcome::CheckFailed(why) => eprintln!("fbuild sync --check: FAILED — {why}"),
        SyncOutcome::LockedFailed(why) => eprintln!("fbuild sync --locked: FAILED — {why}"),
        SyncOutcome::UserCancelled => eprintln!("fbuild sync: cancelled by user"),
        SyncOutcome::Error(why) => eprintln!("fbuild sync: {why}"),
    }
    outcome.exit_code()
}
