//! `fbuild sync` — uv-like dependency sync for PlatformIO projects.
//!
//! FastLED/fbuild#618 Phase 1. Reads `platformio.ini`, classifies every
//! `lib_deps` entry per env, and writes a deterministic JSON
//! `platformio.lock` next to `platformio.ini`.
//!
//! Phase 1 scope:
//!
//! - CLI argument surface — every flag from the issue's proposal.
//! - `lib_deps` parsing + source classification (registry / GitHub / git+
//!   / http-archive / symlink / file / local path).
//! - Deterministic JSON lockfile schema v1.
//! - Multi-env prompt logic (`--yes` bypass, `--check` bypass).
//! - `--check` freshness comparison against an existing lockfile.
//! - `--locked` refuses to write.
//! - `--dry-run` prints planned changes without writing.
//! - Atomic writes via `fbuild_core::fs::write_atomic_sync` (#865).
//!
//! Explicitly deferred to Phase 2 (documented in the PR body):
//!
//! - Actual network resolution of GitHub refs → commit SHAs
//! - PIO registry version → archive URL resolution
//! - Archive sha256 capture during install
//! - Toolchain / platform / framework locking
//! - `HTTP POST /api/sync` daemon endpoint (Phase 1 runs in-process)
//! - Strict build/deploy consumption of the lockfile

pub mod lockfile;
pub mod source;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use fbuild_config::PlatformIOConfig;

use self::lockfile::{LockDiff, Lockfile, LockfileError};
use self::source::{classify, ClassifiedDep};

/// CLI arguments captured from `Commands::Sync`.
#[derive(Debug, Clone, Default)]
pub struct SyncArgs {
    /// `-e <env>` — sync a single environment. Skips the multi-env prompt.
    pub environment: Option<String>,
    /// `--yes` — accept multi-env scope without prompting.
    pub yes: bool,
    /// `--locked` — require a fresh lockfile; install nothing new + never
    /// rewrite the lock. Phase 1 stops at the freshness check (there's
    /// no install path yet).
    pub locked: bool,
    /// `--check` — validate freshness without installing or writing.
    /// Exits non-zero if any selected env is stale/missing.
    pub check: bool,
    /// `--dry-run` — print planned changes; don't write.
    pub dry_run: bool,
    /// `--upgrade` — repin every dep to the current registry/tag latest
    /// (Phase 2). Phase 1 accepts the flag but only re-classifies; no
    /// resolution.
    #[allow(dead_code)] // FastLED/fbuild#618 Phase 2 hook — CLI surface stable now
    pub upgrade: bool,
    /// `--upgrade-package <name>` — repin only the named dep (Phase 2).
    #[allow(dead_code)] // FastLED/fbuild#618 Phase 2 hook — CLI surface stable now
    pub upgrade_package: Option<String>,
    /// Explicit project dir. Defaults to CWD.
    pub project_dir: Option<PathBuf>,
}

impl SyncArgs {
    /// True when caller has opted out of the multi-env prompt (either via
    /// `--yes` or by picking one env explicitly), or when the operation
    /// doesn't need to prompt because it can't install/write anyway
    /// (`--check`).
    pub fn skip_multi_env_prompt(&self) -> bool {
        self.yes || self.environment.is_some() || self.check
    }
}

/// Structured result surfaced to `dispatch`. The runner returns an exit
/// code; extra state is emitted to stderr as it happens.
///
/// Some variants carry payload the CLI adapter doesn't read directly —
/// they're preserved because programmatic callers (tests + future
/// `fbuild sync --json`) will consume the structured output. Clippy's
/// dead-code check doesn't see the tests as consumers, so we allow at
/// the enum level.
#[allow(dead_code)] // Variants populated for structured callers; CLI adapter only reads exit_code.
#[derive(Debug)]
pub enum SyncOutcome {
    /// Lockfile written / re-written successfully. Exit 0.
    Wrote(PathBuf),
    /// Lockfile matches the current `platformio.ini` (nothing to do).
    /// Exit 0.
    NoOp,
    /// `--check` passed — every selected env is fresh in the lock.
    /// Exit 0.
    CheckPassed,
    /// `--check` failed — at least one env is stale. Exit 1.
    CheckFailed(String),
    /// `--locked` failed — lockfile missing or the current inputs
    /// changed. Exit 2.
    LockedFailed(String),
    /// `--dry-run` — nothing written. Exit 0.
    DryRun,
    /// User declined the multi-env prompt. Exit 3.
    UserCancelled,
    /// A hard error during classification / I/O.
    Error(String),
}

impl SyncOutcome {
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Wrote(_) | Self::NoOp | Self::CheckPassed | Self::DryRun => 0,
            Self::CheckFailed(_) => 1,
            Self::LockedFailed(_) => 2,
            Self::UserCancelled => 3,
            Self::Error(_) => 4,
        }
    }
}

/// Errors that can happen during a sync run. Callers usually convert
/// these into a [`SyncOutcome::Error`] before returning to the CLI.
#[derive(Debug)]
pub enum SyncError {
    NoPlatformioIni(PathBuf),
    ConfigParse(String),
    UnknownEnv(String),
    NoEnvsDeclared,
    Lockfile(LockfileError),
    Io(String),
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoPlatformioIni(p) => write!(f, "no platformio.ini at {}", p.display()),
            Self::ConfigParse(m) => write!(f, "platformio.ini parse: {m}"),
            Self::UnknownEnv(e) => write!(f, "environment `{e}` not declared in platformio.ini"),
            Self::NoEnvsDeclared => write!(f, "no [env:*] sections in platformio.ini"),
            Self::Lockfile(e) => write!(f, "{e}"),
            Self::Io(m) => write!(f, "io error: {m}"),
        }
    }
}

impl std::error::Error for SyncError {}

impl From<LockfileError> for SyncError {
    fn from(e: LockfileError) -> Self {
        Self::Lockfile(e)
    }
}

/// Top-level entry. Reads the project's `platformio.ini`, selects envs,
/// classifies deps, runs the flag-specific action, and returns a
/// [`SyncOutcome`] whose exit code the caller propagates.
pub async fn run_sync(args: SyncArgs) -> SyncOutcome {
    match do_run_sync(args).await {
        Ok(o) => o,
        Err(e) => SyncOutcome::Error(e.to_string()),
    }
}

async fn do_run_sync(args: SyncArgs) -> Result<SyncOutcome, SyncError> {
    let project_dir = match &args.project_dir {
        Some(p) => p.clone(),
        None => std::env::current_dir().map_err(|e| SyncError::Io(e.to_string()))?,
    };
    let ini_path = project_dir.join("platformio.ini");
    if !ini_path.is_file() {
        return Err(SyncError::NoPlatformioIni(project_dir));
    }

    let config = PlatformIOConfig::from_path(&ini_path)
        .map_err(|e| SyncError::ConfigParse(e.to_string()))?;

    // Discover envs. `get_environments` returns borrowed slices; own them
    // so the rest of the pipeline can move envs into the classified map.
    let all_envs: Vec<String> = config.get_environments().iter().map(|s| s.to_string()).collect();
    if all_envs.is_empty() {
        return Err(SyncError::NoEnvsDeclared);
    }

    // Env selection.
    let selected_envs = select_envs(&all_envs, args.environment.as_deref())?;

    // Multi-env prompt (skipped by --yes / --check / -e <env>).
    if selected_envs.len() > 1
        && !args.skip_multi_env_prompt()
        && !prompt_multi_env(&selected_envs)
    {
        return Ok(SyncOutcome::UserCancelled);
    }

    // Classify every env's lib_deps.
    let mut classified: BTreeMap<String, Vec<ClassifiedDep>> = BTreeMap::new();
    for env in &selected_envs {
        let deps = config
            .get_lib_deps(env)
            .map_err(|e| SyncError::ConfigParse(format!("env `{env}` lib_deps: {e}")))?;
        classified.insert(env.clone(), deps.iter().map(|d| classify(d)).collect());
    }

    let lock_path = project_dir.join("platformio.lock");

    // --check: freshness only.
    if args.check {
        return Ok(run_check(&lock_path, &classified));
    }

    // --locked: freshness + refuse to write.
    if args.locked {
        return Ok(run_locked(&lock_path, &classified));
    }

    // --dry-run: print the intent + planned lock summary; don't write.
    if args.dry_run {
        print_planned_summary(&classified);
        return Ok(SyncOutcome::DryRun);
    }

    // Normal write. Compare first; if a matching lock already exists,
    // report NoOp so we don't touch mtime for identical inputs.
    if let Ok(existing) = Lockfile::read(&lock_path) {
        if existing.compare_to_classified(&classified) == LockDiff::Fresh {
            eprintln!("fbuild sync: lock is fresh ({})", lock_path.display());
            return Ok(SyncOutcome::NoOp);
        }
    }

    let now = utc_iso_seconds();
    let lock = Lockfile::from_classified(now, classified);
    lock.write_atomic(&lock_path)?;
    eprintln!("fbuild sync: wrote {}", lock_path.display());
    Ok(SyncOutcome::Wrote(lock_path))
}

// ---------- helpers ----------

fn select_envs(all: &[String], selection: Option<&str>) -> Result<Vec<String>, SyncError> {
    match selection {
        Some(e) => {
            if all.iter().any(|x| x == e) {
                Ok(vec![e.to_string()])
            } else {
                Err(SyncError::UnknownEnv(e.to_string()))
            }
        }
        None => {
            let mut list: Vec<String> = all.to_vec();
            list.sort();
            Ok(list)
        }
    }
}

/// Interactive multi-env prompt. Returns `true` to proceed. Reads a
/// single line from stdin — writes to stderr so pipelines that consume
/// stdout aren't corrupted.
fn prompt_multi_env(envs: &[String]) -> bool {
    use std::io::{BufRead, Write};
    eprintln!(
        "fbuild sync will resolve {} environments: {}",
        envs.len(),
        envs.join(", ")
    );
    eprint!("Proceed? [y/N] ");
    let _ = std::io::stderr().flush();
    let stdin = std::io::stdin();
    let mut line = String::new();
    if stdin.lock().read_line(&mut line).is_err() {
        return false;
    }
    let trimmed = line.trim().to_ascii_lowercase();
    matches!(trimmed.as_str(), "y" | "yes")
}

fn run_check(lock_path: &Path, classified: &BTreeMap<String, Vec<ClassifiedDep>>) -> SyncOutcome {
    let existing = match Lockfile::read(lock_path) {
        Ok(l) => l,
        Err(e) => return SyncOutcome::CheckFailed(format!("no fresh lock: {e}")),
    };
    match existing.compare_to_classified(classified) {
        LockDiff::Fresh => SyncOutcome::CheckPassed,
        LockDiff::Stale(why) => SyncOutcome::CheckFailed(why),
    }
}

fn run_locked(lock_path: &Path, classified: &BTreeMap<String, Vec<ClassifiedDep>>) -> SyncOutcome {
    let existing = match Lockfile::read(lock_path) {
        Ok(l) => l,
        Err(e) => return SyncOutcome::LockedFailed(format!("no lock present: {e}")),
    };
    match existing.compare_to_classified(classified) {
        LockDiff::Fresh => SyncOutcome::NoOp,
        LockDiff::Stale(why) => SyncOutcome::LockedFailed(format!(
            "--locked: lock does not match current platformio.ini ({why})"
        )),
    }
}

fn print_planned_summary(classified: &BTreeMap<String, Vec<ClassifiedDep>>) {
    for (env, deps) in classified {
        eprintln!("[dry-run] env {env}: {} dep(s)", deps.len());
        for d in deps {
            eprintln!(
                "  - {} [{}] status={:?}",
                d.name,
                format!("{:?}", d.source_type).to_ascii_lowercase(),
                d.phase1_lock_status()
            );
        }
    }
}

fn utc_iso_seconds() -> String {
    // Minimal ISO-8601 UTC formatter, trimmed to seconds. Avoids pulling
    // in `chrono` for one string. Format: `YYYY-MM-DDThh:mm:ssZ`.
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_epoch_utc(secs)
}

fn format_epoch_utc(secs: u64) -> String {
    let days_since_epoch = secs / 86_400;
    let mut rem = secs % 86_400;
    let hour = rem / 3600;
    rem %= 3600;
    let minute = rem / 60;
    let second = rem % 60;
    let (year, month, day) = ymd_from_days_since_epoch(days_since_epoch);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Convert days since 1970-01-01 UTC to (year, month, day). Howard
/// Hinnant's public-domain "days_from_civil" algorithm, inverted.
fn ymd_from_days_since_epoch(days: u64) -> (i64, u32, u32) {
    let days = days as i64;
    // Shift epoch reference to 0000-03-01 so month math is trivial.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day)
}

// ---------- tests ----------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_ini(dir: &Path, contents: &str) {
        std::fs::write(dir.join("platformio.ini"), contents).unwrap();
    }

    fn args_for(dir: &Path) -> SyncArgs {
        SyncArgs {
            project_dir: Some(dir.to_path_buf()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn missing_platformio_ini_is_error() {
        let tmp = tempdir().unwrap();
        let outcome = run_sync(args_for(tmp.path())).await;
        assert!(matches!(outcome, SyncOutcome::Error(_)), "got {outcome:?}");
    }

    #[tokio::test]
    async fn single_env_writes_lockfile() {
        let tmp = tempdir().unwrap();
        write_ini(
            tmp.path(),
            r#"[env:uno]
platform = atmelavr
board = uno
lib_deps = FastLED@^3.5.0
"#,
        );
        let mut args = args_for(tmp.path());
        args.yes = true; // Not multi-env; harmless.
        let outcome = run_sync(args).await;
        assert!(matches!(outcome, SyncOutcome::Wrote(_)), "got {outcome:?}");
        assert!(tmp.path().join("platformio.lock").is_file());
    }

    #[tokio::test]
    async fn check_on_missing_lock_returns_failed() {
        let tmp = tempdir().unwrap();
        write_ini(
            tmp.path(),
            r#"[env:uno]
platform = atmelavr
board = uno
lib_deps = FastLED
"#,
        );
        let mut args = args_for(tmp.path());
        args.check = true;
        let outcome = run_sync(args).await;
        assert!(matches!(outcome, SyncOutcome::CheckFailed(_)), "got {outcome:?}");
    }

    #[tokio::test]
    async fn check_on_fresh_lock_passes() {
        let tmp = tempdir().unwrap();
        write_ini(
            tmp.path(),
            r#"[env:uno]
platform = atmelavr
board = uno
lib_deps = FastLED
"#,
        );
        // First run writes the lock.
        let mut a1 = args_for(tmp.path());
        a1.yes = true;
        assert!(matches!(run_sync(a1).await, SyncOutcome::Wrote(_)));
        // Second run in --check mode sees a fresh lock.
        let mut a2 = args_for(tmp.path());
        a2.check = true;
        assert!(matches!(run_sync(a2).await, SyncOutcome::CheckPassed));
    }

    #[tokio::test]
    async fn locked_on_stale_lock_returns_failed() {
        let tmp = tempdir().unwrap();
        write_ini(
            tmp.path(),
            r#"[env:uno]
platform = atmelavr
board = uno
lib_deps = FastLED
"#,
        );
        let mut a1 = args_for(tmp.path());
        a1.yes = true;
        assert!(matches!(run_sync(a1).await, SyncOutcome::Wrote(_)));
        // Change the ini so the lock is now stale.
        write_ini(
            tmp.path(),
            r#"[env:uno]
platform = atmelavr
board = uno
lib_deps =
    FastLED
    NewLib@^1.0
"#,
        );
        let mut a2 = args_for(tmp.path());
        a2.locked = true;
        assert!(matches!(run_sync(a2).await, SyncOutcome::LockedFailed(_)));
    }

    #[tokio::test]
    async fn dry_run_does_not_write_lockfile() {
        let tmp = tempdir().unwrap();
        write_ini(
            tmp.path(),
            r#"[env:uno]
platform = atmelavr
board = uno
lib_deps = FastLED
"#,
        );
        let mut a = args_for(tmp.path());
        a.dry_run = true;
        assert!(matches!(run_sync(a).await, SyncOutcome::DryRun));
        assert!(!tmp.path().join("platformio.lock").is_file());
    }

    #[tokio::test]
    async fn env_selection_by_name_skips_prompt() {
        let tmp = tempdir().unwrap();
        write_ini(
            tmp.path(),
            r#"[env:uno]
platform = atmelavr
board = uno

[env:esp32]
platform = espressif32
board = esp32dev
"#,
        );
        let mut a = args_for(tmp.path());
        a.environment = Some("uno".to_string());
        // Multi-env total, but `-e uno` picks one and skips prompt.
        let outcome = run_sync(a).await;
        assert!(matches!(outcome, SyncOutcome::Wrote(_)), "got {outcome:?}");
    }

    #[tokio::test]
    async fn env_selection_unknown_is_error() {
        let tmp = tempdir().unwrap();
        write_ini(
            tmp.path(),
            r#"[env:uno]
platform = atmelavr
board = uno
"#,
        );
        let mut a = args_for(tmp.path());
        a.environment = Some("nonexistent".to_string());
        let outcome = run_sync(a).await;
        assert!(matches!(outcome, SyncOutcome::Error(_)), "got {outcome:?}");
    }

    #[tokio::test]
    async fn no_op_when_lock_matches_ini() {
        let tmp = tempdir().unwrap();
        write_ini(
            tmp.path(),
            r#"[env:uno]
platform = atmelavr
board = uno
lib_deps = FastLED
"#,
        );
        let mut a1 = args_for(tmp.path());
        a1.yes = true;
        assert!(matches!(run_sync(a1).await, SyncOutcome::Wrote(_)));
        // Run again without changes.
        let mut a2 = args_for(tmp.path());
        a2.yes = true;
        assert!(matches!(run_sync(a2).await, SyncOutcome::NoOp));
    }

    #[test]
    fn exit_code_matrix() {
        assert_eq!(SyncOutcome::Wrote(PathBuf::new()).exit_code(), 0);
        assert_eq!(SyncOutcome::NoOp.exit_code(), 0);
        assert_eq!(SyncOutcome::CheckPassed.exit_code(), 0);
        assert_eq!(SyncOutcome::DryRun.exit_code(), 0);
        assert_eq!(SyncOutcome::CheckFailed("x".into()).exit_code(), 1);
        assert_eq!(SyncOutcome::LockedFailed("x".into()).exit_code(), 2);
        assert_eq!(SyncOutcome::UserCancelled.exit_code(), 3);
        assert_eq!(SyncOutcome::Error("x".into()).exit_code(), 4);
    }

    #[test]
    fn skip_multi_env_prompt_matrix() {
        assert!(SyncArgs { yes: true, ..Default::default() }.skip_multi_env_prompt());
        assert!(SyncArgs {
            environment: Some("uno".into()),
            ..Default::default()
        }
        .skip_multi_env_prompt());
        assert!(SyncArgs { check: true, ..Default::default() }.skip_multi_env_prompt());
        assert!(!SyncArgs::default().skip_multi_env_prompt());
    }

    #[test]
    fn iso_epoch_formatter_basic() {
        // 1970-01-01T00:00:00Z
        assert_eq!(format_epoch_utc(0), "1970-01-01T00:00:00Z");
        // 2000-03-01T00:00:00Z — Y2K, March 1 start-of-day
        assert_eq!(format_epoch_utc(951_868_800), "2000-03-01T00:00:00Z");
        // 2026-01-01T00:00:00Z — round year boundary
        assert_eq!(format_epoch_utc(1_767_225_600), "2026-01-01T00:00:00Z");
    }

    #[test]
    fn iso_epoch_formatter_time_of_day() {
        // 1970-01-01T14:30:45Z = 14*3600 + 30*60 + 45 = 52245 s
        assert_eq!(format_epoch_utc(52_245), "1970-01-01T14:30:45Z");
    }
}
