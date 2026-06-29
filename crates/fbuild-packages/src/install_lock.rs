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

fn write_lock_owner(lock_dir: &Path, package_name: &str, package_version: &str) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(lock_dir.join("owner.txt"))?;
    writeln!(
        file,
        "pid={}\npackage={}\nversion={}\nstarted_unix_nanos={}",
        std::process::id(),
        package_name,
        package_version,
        unique_suffix()
    )?;
    Ok(())
}

fn lock_is_stale(lock_dir: &Path, stale_after: Duration) -> bool {
    let Ok(metadata) = std::fs::metadata(lock_dir) else {
        return true;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    modified
        .elapsed()
        .map(|age| age > stale_after)
        .unwrap_or(false)
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
