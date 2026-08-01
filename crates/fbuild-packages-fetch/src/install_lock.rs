use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fbuild_core::install_status::{self, InstallPhase, InstallRole};
use fbuild_core::{FbuildError, Result};

/// Default ceiling on how old a sibling install-lock can be before a
/// waiter considers it stale and tears it down. Two hours covers the
/// worst-case legit toolchain install on a slow first-build behind a
/// flaky CDN. With FastLED/fbuild#805's per-request HTTP timeouts now
/// in place every download has its own 5 min total deadline, so this
/// ceiling is mostly defense-in-depth; CI runners can shorten it via
/// the `FBUILD_INSTALL_LOCK_STALE_SECS` env var when the job's own
/// wall-clock budget is tighter than 2 h.
const INSTALL_LOCK_STALE_AFTER: Duration = Duration::from_secs(2 * 60 * 60);
const INSTALL_LOCK_POLL: Duration = Duration::from_millis(250);

/// Read the install-lock staleness ceiling, honoring the
/// `FBUILD_INSTALL_LOCK_STALE_SECS` env var override (FastLED/fbuild#805).
///
/// CI jobs whose wall-clock budget is shorter than the 2 h default can
/// set e.g. `FBUILD_INSTALL_LOCK_STALE_SECS=600` so a wedged peer's
/// lock is reclaimed inside the job timeout. Invalid / non-positive
/// values fall back to the compile-time default.
fn install_lock_stale_after() -> Duration {
    if let Ok(s) = std::env::var("FBUILD_INSTALL_LOCK_STALE_SECS") {
        if let Ok(n) = s.parse::<u64>() {
            if n > 0 {
                return Duration::from_secs(n);
            }
        }
    }
    INSTALL_LOCK_STALE_AFTER
}

pub(crate) async fn acquire_for_install(
    install_path: &Path,
    package_name: &str,
    package_version: &str,
) -> Result<InstallLockGuard> {
    acquire_install_lock_at(
        &install_lock_dir(install_path)?,
        package_name,
        package_version,
        install_lock_stale_after(),
        INSTALL_LOCK_POLL,
    )
    .await
}

fn install_lock_dir(install_path: &Path) -> Result<PathBuf> {
    let parent = install_path.parent().ok_or_else(|| {
        FbuildError::PackageError(format!(
            "install path has no parent: {}",
            install_path.display()
        ))
    })?;
    let file_name = install_path
        .file_name()
        .ok_or_else(|| {
            FbuildError::PackageError(format!(
                "install path has no final component: {}",
                install_path.display()
            ))
        })?
        .to_string_lossy();
    Ok(parent.join(format!(".{file_name}.install.lock")))
}

async fn acquire_install_lock_at(
    lock_dir: &Path,
    package_name: &str,
    package_version: &str,
    stale_after: Duration,
    poll: Duration,
) -> Result<InstallLockGuard> {
    let started = Instant::now();
    let mut logged_wait = false;
    loop {
        match std::fs::create_dir(lock_dir) {
            Ok(()) => {
                if let Err(e) = write_lock_owner(lock_dir, package_name, package_version) {
                    let _ = std::fs::remove_dir_all(lock_dir);
                    return Err(e);
                }
                if logged_wait {
                    tracing::info!(
                        "fbuild: acquired install lock for {} {} after waiting {:?}",
                        package_name,
                        package_version,
                        started.elapsed()
                    );
                }
                return Ok(InstallLockGuard {
                    path: lock_dir.to_path_buf(),
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if lock_is_stale(lock_dir, stale_after) {
                    tracing::warn!(
                        "fbuild: removing stale install lock for {} {} at {}",
                        package_name,
                        package_version,
                        lock_dir.display()
                    );
                    if let Err(e) = std::fs::remove_dir_all(lock_dir) {
                        return Err(FbuildError::PackageError(format!(
                            "failed to remove stale install lock {}: {e}",
                            lock_dir.display()
                        )));
                    }
                    logged_wait = false;
                    continue;
                }
                if !logged_wait {
                    install_status::publish_install_status(install_status::status(
                        package_name,
                        Some(package_version),
                        InstallPhase::WaitingForLock,
                        InstallRole::Waiter,
                        format!(
                            "waiting for another process to install {} {}",
                            package_name, package_version
                        ),
                        Some(lock_dir.display().to_string()),
                    ));
                    tracing::info!(
                        "fbuild: waiting for another process to install {} {}",
                        package_name,
                        package_version
                    );
                    logged_wait = true;
                }
                tokio::time::sleep(poll).await;
            }
            Err(e) => {
                return Err(FbuildError::PackageError(format!(
                    "failed to acquire install lock {}: {e}",
                    lock_dir.display()
                )));
            }
        }
    }
}

/// Grace period for a lock directory that has no readable `owner.txt`.
///
/// `create_dir` and [`write_lock_owner`] are two steps, so a waiter can
/// legitimately observe the directory in between. Anything older than this
/// without an owner record was abandoned mid-creation (the writer died
/// between the two calls) and would otherwise wedge until the 2 h ceiling.
const MISSING_OWNER_GRACE: Duration = Duration::from_secs(30);

fn write_lock_owner(lock_dir: &Path, package_name: &str, package_version: &str) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(lock_dir.join("owner.txt"))?;
    writeln!(
        file,
        "pid={}\nexe_stem={}\npackage={}\nversion={}\nstarted_unix_nanos={}",
        std::process::id(),
        current_exe_stem().unwrap_or_default(),
        package_name,
        package_version,
        unique_suffix()
    )?;
    Ok(())
}

/// File stem of the running executable, used to make the liveness probe
/// PID-recycling-safe.
fn current_exe_stem() -> Option<String> {
    std::env::current_exe()
        .ok()?
        .file_stem()
        .and_then(|s| s.to_str())
        .map(str::to_string)
}

/// Owner record parsed out of a lock directory's `owner.txt`.
struct LockOwner {
    pid: Option<u32>,
    exe_stem: Option<String>,
}

fn read_lock_owner(lock_dir: &Path) -> Option<LockOwner> {
    let raw = std::fs::read_to_string(lock_dir.join("owner.txt")).ok()?;
    let mut pid = None;
    let mut exe_stem = None;
    for line in raw.lines() {
        if let Some(v) = line.strip_prefix("pid=") {
            pid = v.trim().parse::<u32>().ok();
        } else if let Some(v) = line.strip_prefix("exe_stem=") {
            let v = v.trim();
            if !v.is_empty() {
                exe_stem = Some(v.to_string());
            }
        }
    }
    Some(LockOwner { pid, exe_stem })
}

/// Is the process that created this lock gone?
///
/// Returns `false` unless we have positive evidence of death — an
/// uninspectable owner is treated as alive so a live install is never torn
/// out from under itself.
///
/// PID recycling is handled by also comparing the recorded executable stem:
/// if the PID is alive but is now some unrelated program, the original owner
/// is gone. When the record predates the `exe_stem` field, liveness alone is
/// used (the old behavior, minus the deadlock).
fn owner_is_dead(owner: &LockOwner) -> bool {
    let Some(pid) = owner.pid else {
        return false;
    };
    if !fbuild_core::process_identity::pid_is_alive(pid) {
        return true;
    }
    match &owner.exe_stem {
        // `pid_exe_stem_matches` fails closed on an uninspectable image, so
        // only treat a *successful* probe of a different program as death.
        Some(stem) => match fbuild_core::process_identity::pid_executable_path(pid) {
            Some(path) => match path.file_stem().and_then(|s| s.to_str()) {
                Some(actual) => !stem_eq(actual, stem),
                None => false,
            },
            None => false,
        },
        None => false,
    }
}

fn stem_eq(left: &str, right: &str) -> bool {
    if cfg!(windows) {
        left.eq_ignore_ascii_case(right)
    } else {
        left == right
    }
}

/// Should a waiter tear this lock down?
///
/// Three independent reasons, in order of confidence:
/// 1. The directory vanished — nothing to wait for.
/// 2. The owning process is gone. This is the FastLED/fbuild#1213 deadlock:
///    the PID was already being written to `owner.txt` and simply never
///    read, so a crashed install wedged every later build until the 2 h
///    ceiling expired.
/// 3. The age ceiling — the pre-existing backstop, kept for the cases PID
///    liveness cannot answer (owner record missing on a foreign filesystem,
///    a genuinely hung but still-running peer).
fn lock_is_stale(lock_dir: &Path, stale_after: Duration) -> bool {
    lock_is_stale_with_grace(lock_dir, stale_after, MISSING_OWNER_GRACE)
}

/// [`lock_is_stale`] with the missing-owner grace injected, so tests can
/// exercise the abandoned-mid-creation branch without sleeping 30 s.
fn lock_is_stale_with_grace(
    lock_dir: &Path,
    stale_after: Duration,
    missing_owner_grace: Duration,
) -> bool {
    let Ok(metadata) = std::fs::metadata(lock_dir) else {
        return true;
    };
    let age = metadata.modified().ok().and_then(|m| m.elapsed().ok());

    match read_lock_owner(lock_dir) {
        Some(owner) => {
            if owner_is_dead(&owner) {
                tracing::warn!(
                    pid = ?owner.pid,
                    lock = %lock_dir.display(),
                    "install lock owner is no longer running; reclaiming"
                );
                return true;
            }
        }
        None => {
            // No owner record. Only meaningful once the create/write window
            // has comfortably passed.
            if age.map(|a| a >= missing_owner_grace).unwrap_or(false) {
                tracing::warn!(
                    lock = %lock_dir.display(),
                    "install lock has no owner record after {:?}; reclaiming",
                    missing_owner_grace
                );
                return true;
            }
            return false;
        }
    }

    age.map(|a| a > stale_after).unwrap_or(false)
}

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

pub(crate) struct InstallLockGuard {
    path: PathBuf,
}

impl Drop for InstallLockGuard {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.path) {
            tracing::warn!(
                "fbuild: failed to remove install lock {}: {}",
                self.path.display(),
                e
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct InstallStatusSubscriberGuard;

    impl Drop for InstallStatusSubscriberGuard {
        fn drop(&mut self) {
            fbuild_core::install_status::clear_install_status_subscriber();
        }
    }

    /// Write a lock directory owned by `pid` with an optional exe stem,
    /// mimicking what a crashed peer leaves behind.
    fn plant_lock(lock_dir: &Path, pid: u32, exe_stem: Option<&str>) {
        std::fs::create_dir_all(lock_dir).unwrap();
        let mut body = format!("pid={pid}\n");
        if let Some(stem) = exe_stem {
            body.push_str(&format!("exe_stem={stem}\n"));
        }
        body.push_str("package=toolchain\nversion=1.0\n");
        std::fs::write(lock_dir.join("owner.txt"), body).unwrap();
    }

    /// A PID that is (almost certainly) not running. PID 0 is never a normal
    /// user process on either platform, and `pid_is_alive` reports it dead.
    const DEAD_PID: u32 = 0;

    /// The FastLED/fbuild#1213 deadlock: a crashed owner used to wedge every
    /// later build for the full 2 h ceiling, even though its PID was already
    /// recorded in `owner.txt` — it was simply never read.
    #[test]
    fn lock_owned_by_a_dead_pid_is_stale_immediately() {
        let tmp = tempfile::TempDir::new().unwrap();
        let lock_dir = tmp.path().join(".1.0.install.lock");
        plant_lock(&lock_dir, DEAD_PID, None);

        assert!(lock_is_stale(&lock_dir, Duration::from_secs(2 * 60 * 60)));
    }

    /// The complement, and the one that matters for safety: a live owner's
    /// lock must never be reclaimed, however long the ceiling is.
    #[test]
    fn lock_owned_by_a_live_pid_is_not_stale() {
        let tmp = tempfile::TempDir::new().unwrap();
        let lock_dir = tmp.path().join(".1.0.install.lock");
        plant_lock(&lock_dir, std::process::id(), current_exe_stem().as_deref());

        assert!(!lock_is_stale(&lock_dir, Duration::from_secs(2 * 60 * 60)));
    }

    /// PID recycling: the recorded PID is alive but is now a different
    /// program, so the original owner is gone.
    #[test]
    fn lock_whose_pid_was_recycled_by_another_program_is_stale() {
        let tmp = tempfile::TempDir::new().unwrap();
        let lock_dir = tmp.path().join(".1.0.install.lock");
        plant_lock(
            &lock_dir,
            std::process::id(),
            Some("definitely-not-this-test-binary"),
        );

        assert!(lock_is_stale(&lock_dir, Duration::from_secs(2 * 60 * 60)));
    }

    /// A record written before the `exe_stem` field existed must still work:
    /// liveness alone decides, which is the pre-#1213 data plus the fix.
    #[test]
    fn legacy_owner_record_without_exe_stem_still_uses_liveness() {
        let tmp = tempfile::TempDir::new().unwrap();
        let live = tmp.path().join(".live.install.lock");
        let dead = tmp.path().join(".dead.install.lock");
        plant_lock(&live, std::process::id(), None);
        plant_lock(&dead, DEAD_PID, None);

        assert!(!lock_is_stale(&live, Duration::from_secs(2 * 60 * 60)));
        assert!(lock_is_stale(&dead, Duration::from_secs(2 * 60 * 60)));
    }

    /// `create_dir` then `write_lock_owner` is two steps; a waiter that
    /// catches the gap must NOT tear down a lock that is being created.
    #[test]
    fn freshly_created_lock_without_owner_record_is_not_stale() {
        let tmp = tempfile::TempDir::new().unwrap();
        let lock_dir = tmp.path().join(".1.0.install.lock");
        std::fs::create_dir_all(&lock_dir).unwrap();

        assert!(!lock_is_stale(&lock_dir, Duration::from_secs(2 * 60 * 60)));
    }

    /// ...but a lock stuck without an owner record past the grace period was
    /// abandoned mid-creation and must be reclaimed rather than waiting out
    /// the 2 h ceiling.
    #[test]
    fn owner_record_missing_past_the_grace_period_is_stale() {
        let tmp = tempfile::TempDir::new().unwrap();
        let lock_dir = tmp.path().join(".1.0.install.lock");
        std::fs::create_dir_all(&lock_dir).unwrap();

        // `lock_is_stale` compares against MISSING_OWNER_GRACE using the
        // directory mtime; a zero grace makes any existing dir qualify
        // without sleeping in the test.
        assert!(super::lock_is_stale_with_grace(
            &lock_dir,
            Duration::from_secs(2 * 60 * 60),
            Duration::ZERO
        ));
    }

    #[test]
    fn written_owner_record_round_trips() {
        let tmp = tempfile::TempDir::new().unwrap();
        let lock_dir = tmp.path().join(".1.0.install.lock");
        std::fs::create_dir_all(&lock_dir).unwrap();
        write_lock_owner(&lock_dir, "toolchain", "1.0").unwrap();

        let owner = read_lock_owner(&lock_dir).expect("owner record");
        assert_eq!(owner.pid, Some(std::process::id()));
        assert_eq!(owner.exe_stem, current_exe_stem());
        assert!(!owner_is_dead(&owner), "this process is alive");
    }

    #[test]
    fn lock_path_is_sibling_of_install_path() {
        let root = Path::new("/cache/toolchains/example/1.0");
        let lock_dir = install_lock_dir(root).unwrap();
        assert_eq!(
            lock_dir,
            Path::new("/cache/toolchains/example/.1.0.install.lock")
        );
    }

    #[tokio::test]
    async fn lock_blocks_second_caller_until_released() {
        let tmp = tempfile::TempDir::new().unwrap();
        let install_path = tmp.path().join("toolchain").join("1.0");
        std::fs::create_dir_all(install_path.parent().unwrap()).unwrap();

        let first = acquire_for_install(&install_path, "toolchain", "1.0")
            .await
            .unwrap();
        let waiter_path = install_path.clone();
        let waiter = tokio::spawn(async move {
            acquire_for_install(&waiter_path, "toolchain", "1.0")
                .await
                .unwrap()
        });

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiter.is_finished());

        drop(first);
        let second = tokio::time::timeout(Duration::from_secs(2), waiter)
            .await
            .unwrap()
            .unwrap();
        drop(second);
    }

    #[tokio::test]
    async fn lock_waiter_publishes_structured_wait_status() {
        let tmp = tempfile::TempDir::new().unwrap();
        let install_path = tmp.path().join("framework").join("3.0");
        std::fs::create_dir_all(install_path.parent().unwrap()).unwrap();
        let lock_dir = install_lock_dir(&install_path).unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_for_callback = Arc::clone(&seen);
        let _subscriber_guard = InstallStatusSubscriberGuard;
        fbuild_core::install_status::set_install_status_subscriber(move |status| {
            seen_for_callback.lock().unwrap().push(status);
        });

        let first = acquire_for_install(&install_path, "framework", "3.0")
            .await
            .unwrap();
        let waiter_path = install_path.clone();
        let waiter = tokio::spawn(async move {
            acquire_for_install(&waiter_path, "framework", "3.0")
                .await
                .unwrap()
        });

        let status = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(status) = {
                    let statuses = seen.lock().unwrap();
                    statuses
                        .iter()
                        .find(|status| {
                            status.name == "framework" && status.version.as_deref() == Some("3.0")
                        })
                        .cloned()
                } {
                    break status;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("timed out waiting for framework waiter status");
        assert_eq!(status.phase, InstallPhase::WaitingForLock);
        assert_eq!(status.role, InstallRole::Waiter);
        assert_eq!(status.lock.as_deref(), Some(lock_dir.to_str().unwrap()));
        assert!(
            status.message.contains("waiting for another process"),
            "unexpected wait message: {}",
            status.message
        );

        drop(first);
        let second = tokio::time::timeout(Duration::from_secs(2), waiter)
            .await
            .unwrap()
            .unwrap();
        drop(second);
    }

    #[test]
    fn install_lock_stale_after_honors_env_override() {
        // FastLED/fbuild#805 MEDIUM: env override for CI runners that
        // can't wait 2 h on a wedged peer. Sequential within this
        // single test so we don't race other tests that read the env.
        let prev = std::env::var("FBUILD_INSTALL_LOCK_STALE_SECS").ok();
        std::env::set_var("FBUILD_INSTALL_LOCK_STALE_SECS", "600");
        assert_eq!(install_lock_stale_after(), Duration::from_secs(600));
        // Garbage value falls back to default.
        std::env::set_var("FBUILD_INSTALL_LOCK_STALE_SECS", "not-a-number");
        assert_eq!(install_lock_stale_after(), INSTALL_LOCK_STALE_AFTER);
        // Zero falls back to default (zero would make every lock instantly stale).
        std::env::set_var("FBUILD_INSTALL_LOCK_STALE_SECS", "0");
        assert_eq!(install_lock_stale_after(), INSTALL_LOCK_STALE_AFTER);
        // Unset → default.
        std::env::remove_var("FBUILD_INSTALL_LOCK_STALE_SECS");
        assert_eq!(install_lock_stale_after(), INSTALL_LOCK_STALE_AFTER);
        // Restore prior value if any.
        if let Some(v) = prev {
            std::env::set_var("FBUILD_INSTALL_LOCK_STALE_SECS", v);
        }
    }

    #[tokio::test]
    async fn lock_recovers_stale_lock_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let install_path = tmp.path().join("platform").join("2.0");
        std::fs::create_dir_all(install_path.parent().unwrap()).unwrap();
        let lock_dir = install_lock_dir(&install_path).unwrap();
        std::fs::create_dir(&lock_dir).unwrap();

        let guard = acquire_install_lock_at(
            &lock_dir,
            "platform",
            "2.0",
            Duration::ZERO,
            Duration::from_millis(1),
        )
        .await
        .unwrap();

        assert!(lock_dir.join("owner.txt").is_file());
        drop(guard);
        assert!(!lock_dir.exists());
    }
}
