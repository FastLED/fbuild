//! Internal managed zccache binary.
//!
//! fbuild downloads its own pinned zccache release into
//! `~/.fbuild/<mode>/bin/zccache-<ver>/` and runs that binary, instead of
//! resolving whatever `zccache` happens to be installed in the ambient
//! Python environment. Pinning a hard zccache version as a `pyproject`
//! dependency forces every package that shares the venv to agree on one
//! version; embedding the binary here removes fbuild from that
//! dependency-resolution tug-of-war.
//!
//! Resolution is owned by [`crate::zccache::find_zccache`]; this module
//! provides [`ensure`], which guarantees the pinned binaries are on disk
//! (downloading + verifying once) and returns the path to the `zccache`
//! CLI.

use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fbuild_core::install_status::{self, InstallPhase, InstallRole};
use fbuild_core::{FbuildError, Result};

/// The zccache release fbuild pins. Bump in lockstep with the floor that
/// the rest of the toolchain expects.
pub const MANAGED_ZCCACHE_VERSION: &str = "1.12.10";

/// GitHub release tag for [`MANAGED_ZCCACHE_VERSION`]. The zccache release
/// workflow tags without a `v` prefix, while the per-asset filenames carry
/// `v<version>` — keep both spellings in sync when bumping.
const RELEASE_TAG: &str = "1.12.10";

/// Source repository for managed downloads.
const REPO: &str = "zackees/zccache";

/// Binaries shipped inside every zccache release archive. fbuild only
/// invokes `zccache` directly, but `zccache start` spawns `zccache-daemon`,
/// so all three must land side by side.
const ZCCACHE_BINARIES: [&str; 3] = ["zccache", "zccache-daemon", "zccache-fp"];
const INSTALL_LOCK_STALE_AFTER: Duration = Duration::from_secs(15 * 60);
const INSTALL_LOCK_POLL: Duration = Duration::from_millis(250);

/// Platform binary suffix.
fn binary_ext() -> &'static str {
    if cfg!(windows) {
        ".exe"
    } else {
        ""
    }
}

/// Directory holding the managed binaries: `~/.fbuild/<mode>/bin/zccache-<ver>/`.
pub fn managed_dir() -> PathBuf {
    fbuild_paths::get_fbuild_root()
        .join("bin")
        .join(format!("zccache-{MANAGED_ZCCACHE_VERSION}"))
}

/// Path to the managed `zccache` CLI binary (whether or not it exists yet).
pub fn managed_zccache_exe() -> PathBuf {
    managed_dir().join(format!("zccache{}", binary_ext()))
}

/// Ensure the pinned zccache is present on disk, downloading + verifying it
/// once if needed, and return the path to the `zccache` CLI binary.
///
/// The download runs on a dedicated thread: the build orchestrators call
/// this from inside the daemon's Tokio runtime, and `reqwest::blocking`
/// panics if it constructs its current-thread runtime while another runtime
/// is already active on the calling thread.
pub fn ensure() -> Result<PathBuf> {
    let exe = managed_zccache_exe();
    if exe.is_file() {
        return Ok(exe);
    }

    std::thread::spawn(download_and_install)
        .join()
        .map_err(|_| {
            FbuildError::Other("managed zccache download thread panicked".to_string())
        })??;

    if !exe.is_file() {
        return Err(FbuildError::Other(format!(
            "managed zccache missing after install: {}",
            exe.display()
        )));
    }
    Ok(exe)
}

fn download_and_install() -> Result<()> {
    let final_dir = managed_dir();
    let parent = final_dir
        .parent()
        .ok_or_else(|| FbuildError::Other("managed zccache dir has no parent".to_string()))?;
    std::fs::create_dir_all(parent)?;

    let _install_lock = acquire_install_lock(parent)?;
    cleanup_stale_install_staging(parent, INSTALL_LOCK_STALE_AFTER)?;
    if managed_zccache_exe().is_file() {
        return Ok(());
    }

    let triple = host_triple()?;
    let archive_ext = if cfg!(windows) { "zip" } else { "tar.gz" };
    let asset = format!("zccache-v{MANAGED_ZCCACHE_VERSION}-{triple}.{archive_ext}");
    let base = format!("https://github.com/{REPO}/releases/download/{RELEASE_TAG}");

    let client = http_client()?;

    let sums = http_get_text(&client, &format!("{base}/SHA256SUMS"))?;
    let expected = sha256_for_asset(&sums, &asset)
        .ok_or_else(|| FbuildError::Other(format!("SHA256SUMS has no entry for {asset}")))?;

    publish_status(
        InstallPhase::Downloading,
        InstallRole::Installer,
        format!("downloading managed zccache {MANAGED_ZCCACHE_VERSION}"),
        None,
    );
    tracing::info!("fbuild: downloading managed zccache {MANAGED_ZCCACHE_VERSION} ({asset})");
    let bytes = http_get_bytes(&client, &format!("{base}/{asset}"))?;

    publish_status(
        InstallPhase::Verifying,
        InstallRole::Installer,
        format!("verifying managed zccache {MANAGED_ZCCACHE_VERSION}"),
        None,
    );
    let actual = sha256_hex(&bytes);
    if !actual.eq_ignore_ascii_case(&expected) {
        return Err(FbuildError::Other(format!(
            "managed zccache checksum mismatch for {asset}: expected {expected}, got {actual}"
        )));
    }

    // Extract into a unique sibling temp dir, then atomically rename into
    // place so a concurrent fbuild process never observes a half-extracted
    // directory.
    let staging = parent.join(format!(
        ".zccache-{}-{}.tmp",
        std::process::id(),
        unique_suffix()
    ));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;

    publish_status(
        InstallPhase::Extracting,
        InstallRole::Installer,
        format!("extracting managed zccache {MANAGED_ZCCACHE_VERSION}"),
        None,
    );
    let extracted = if archive_ext == "zip" {
        extract_zip(&bytes, &staging)
    } else {
        extract_tar_gz(&bytes, &staging)
    };
    if let Err(e) = extracted {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }

    #[cfg(unix)]
    if let Err(e) = set_executable(&staging) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }

    match std::fs::rename(&staging, &final_dir) {
        Ok(()) => {
            publish_status(
                InstallPhase::Installed,
                InstallRole::Installer,
                format!("installed managed zccache {MANAGED_ZCCACHE_VERSION}"),
                None,
            );
            Ok(())
        }
        // Lost a race with a concurrent installer — the winner's directory
        // is already in place, so discard our staging copy and succeed.
        Err(_) if final_dir.join(format!("zccache{}", binary_ext())).is_file() => {
            let _ = std::fs::remove_dir_all(&staging);
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            Err(FbuildError::Other(format!(
                "failed to install managed zccache into {}: {e}",
                final_dir.display()
            )))
        }
    }
}

fn install_lock_dir(parent: &Path) -> PathBuf {
    parent.join(format!(".zccache-{MANAGED_ZCCACHE_VERSION}.install.lock"))
}

fn cleanup_stale_install_staging(parent: &Path, stale_after: Duration) -> Result<usize> {
    let mut removed = 0;
    let entries = std::fs::read_dir(parent).map_err(|e| {
        FbuildError::Other(format!(
            "failed to scan managed zccache install dir {}: {e}",
            parent.display()
        ))
    })?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if !is_zccache_staging_dir(&path) || !path.is_dir() {
            continue;
        }
        if path_is_stale(&path, stale_after) {
            tracing::warn!(
                "fbuild: removing stale managed zccache staging dir {}",
                path.display()
            );
            std::fs::remove_dir_all(&path).map_err(|e| {
                FbuildError::Other(format!(
                    "failed to remove stale managed zccache staging dir {}: {e}",
                    path.display()
                ))
            })?;
            removed += 1;
        }
    }

    Ok(removed)
}

fn is_zccache_staging_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.starts_with(".zccache-") && name.ends_with(".tmp"))
        .unwrap_or(false)
}

fn acquire_install_lock(parent: &Path) -> Result<InstallLockGuard> {
    acquire_install_lock_at(
        &install_lock_dir(parent),
        INSTALL_LOCK_STALE_AFTER,
        INSTALL_LOCK_POLL,
    )
}

fn acquire_install_lock_at(
    lock_dir: &Path,
    stale_after: Duration,
    poll: Duration,
) -> Result<InstallLockGuard> {
    let started = Instant::now();
    let mut logged_wait = false;
    loop {
        match std::fs::create_dir(lock_dir) {
            Ok(()) => {
                if let Err(e) = write_lock_owner(lock_dir) {
                    let _ = std::fs::remove_dir_all(lock_dir);
                    return Err(e);
                }
                if started.elapsed() > poll {
                    tracing::info!(
                        "fbuild: acquired managed zccache install lock after waiting {:?}",
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
                        "fbuild: removing stale managed zccache install lock {}",
                        lock_dir.display()
                    );
                    if let Err(e) = std::fs::remove_dir_all(lock_dir) {
                        return Err(FbuildError::Other(format!(
                            "failed to remove stale managed zccache install lock {}: {e}",
                            lock_dir.display()
                        )));
                    }
                    logged_wait = false;
                    continue;
                }
                if !logged_wait {
                    tracing::info!(
                        "fbuild: waiting for another process to install managed zccache {}",
                        MANAGED_ZCCACHE_VERSION
                    );
                    publish_status(
                        InstallPhase::WaitingForLock,
                        InstallRole::Waiter,
                        format!(
                            "waiting for another process to install managed zccache {MANAGED_ZCCACHE_VERSION}"
                        ),
                        Some(lock_dir.display().to_string()),
                    );
                    logged_wait = true;
                }
                std::thread::sleep(poll);
            }
            Err(e) => {
                return Err(FbuildError::Other(format!(
                    "failed to acquire managed zccache install lock {}: {e}",
                    lock_dir.display()
                )));
            }
        }
    }
}

fn publish_status(phase: InstallPhase, role: InstallRole, message: String, lock: Option<String>) {
    install_status::publish_install_status(install_status::status(
        "zccache",
        Some(MANAGED_ZCCACHE_VERSION),
        phase,
        role,
        message,
        lock,
    ));
}

fn write_lock_owner(lock_dir: &Path) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(lock_dir.join("owner.txt"))?;
    writeln!(
        file,
        "pid={}\nversion={}\nstarted_unix_nanos={}",
        std::process::id(),
        MANAGED_ZCCACHE_VERSION,
        unique_suffix()
    )?;
    Ok(())
}

fn lock_is_stale(lock_dir: &Path, stale_after: Duration) -> bool {
    path_is_stale(lock_dir, stale_after)
}

fn path_is_stale(path: &Path, stale_after: Duration) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
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

struct InstallLockGuard {
    path: PathBuf,
}

impl Drop for InstallLockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Map the running host to the release triple zccache publishes for it.
fn host_triple() -> Result<&'static str> {
    Ok(match (std::env::consts::ARCH, std::env::consts::OS) {
        ("x86_64", "windows") => "x86_64-pc-windows-msvc",
        ("aarch64", "windows") => "aarch64-pc-windows-msvc",
        ("x86_64", "macos") => "x86_64-apple-darwin",
        ("aarch64", "macos") => "aarch64-apple-darwin",
        // zccache ships static musl Linux builds; they run on glibc hosts.
        ("x86_64", "linux") => "x86_64-unknown-linux-musl",
        ("aarch64", "linux") => "aarch64-unknown-linux-musl",
        (arch, os) => {
            return Err(FbuildError::Other(format!(
                "no managed zccache build for {arch}-{os}"
            )))
        }
    })
}

fn http_client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .user_agent(concat!("fbuild/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| FbuildError::Other(format!("failed to build http client: {e}")))
}

fn http_get_bytes(client: &reqwest::blocking::Client, url: &str) -> Result<Vec<u8>> {
    let resp = client
        .get(url)
        .send()
        .map_err(|e| FbuildError::Other(format!("download failed for {url}: {e}")))?;
    if !resp.status().is_success() {
        return Err(FbuildError::Other(format!(
            "download failed for {url}: HTTP {}",
            resp.status()
        )));
    }
    resp.bytes()
        .map(|b| b.to_vec())
        .map_err(|e| FbuildError::Other(format!("reading body of {url} failed: {e}")))
}

fn http_get_text(client: &reqwest::blocking::Client, url: &str) -> Result<String> {
    let resp = client
        .get(url)
        .send()
        .map_err(|e| FbuildError::Other(format!("download failed for {url}: {e}")))?;
    if !resp.status().is_success() {
        return Err(FbuildError::Other(format!(
            "download failed for {url}: HTTP {}",
            resp.status()
        )));
    }
    resp.text()
        .map_err(|e| FbuildError::Other(format!("reading body of {url} failed: {e}")))
}

/// Look up the checksum for `asset` in a `SHA256SUMS` body.
///
/// Lines are `<hex>  ./<asset>`; the leading `./` is optional.
fn sha256_for_asset(sums: &str, asset: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let mut it = line.split_whitespace();
        let hash = it.next()?;
        let name = it.next()?.trim_start_matches("./");
        (name == asset).then(|| hash.to_string())
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    to_hex(&hasher.finalize())
}

fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Desired on-disk binary filenames for this platform.
fn desired_filenames() -> Vec<String> {
    let ext = binary_ext();
    ZCCACHE_BINARIES
        .iter()
        .map(|name| format!("{name}{ext}"))
        .collect()
}

/// Match an archive entry's basename to a desired binary filename,
/// tolerating a `.exe` difference between archive and host conventions.
fn match_desired<'a>(file_name: &str, desired: &'a [String]) -> Option<&'a String> {
    desired
        .iter()
        .find(|d| d.as_str() == file_name || d.trim_end_matches(".exe") == file_name)
}

fn extract_zip(data: &[u8], dest: &Path) -> Result<()> {
    let desired = desired_filenames();
    let reader = std::io::Cursor::new(data);
    let mut archive =
        zip::ZipArchive::new(reader).map_err(|e| FbuildError::Other(format!("zip open: {e}")))?;
    let mut found = BTreeSet::new();

    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| FbuildError::Other(format!("zip entry: {e}")))?;
        if file.is_dir() {
            continue;
        }
        let file_name = Path::new(file.name())
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("")
            .to_string();
        if let Some(target) = match_desired(&file_name, &desired) {
            let mut out = std::fs::File::create(dest.join(target))?;
            std::io::copy(&mut file, &mut out)?;
            found.insert(target.clone());
        }
    }

    ensure_all_found(&desired, &found)
}

fn extract_tar_gz(data: &[u8], dest: &Path) -> Result<()> {
    let desired = desired_filenames();
    let reader = std::io::Cursor::new(data);
    let gz = flate2::read::GzDecoder::new(reader);
    let mut archive = tar::Archive::new(gz);
    let mut found = BTreeSet::new();

    for entry in archive
        .entries()
        .map_err(|e| FbuildError::Other(format!("tar entries: {e}")))?
    {
        let mut entry = entry.map_err(|e| FbuildError::Other(format!("tar entry: {e}")))?;
        let file_name = entry
            .path()
            .map_err(|e| FbuildError::Other(format!("tar path: {e}")))?
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("")
            .to_string();
        if let Some(target) = match_desired(&file_name, &desired) {
            let mut out = std::fs::File::create(dest.join(target))?;
            std::io::copy(&mut entry, &mut out)?;
            found.insert(target.clone());
        }
    }

    ensure_all_found(&desired, &found)
}

fn ensure_all_found(desired: &[String], found: &BTreeSet<String>) -> Result<()> {
    let missing: Vec<String> = desired
        .iter()
        .filter(|d| !found.contains(*d))
        .cloned()
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(FbuildError::Other(format!(
            "managed zccache archive missing binaries: {}",
            missing.join(", ")
        )))
    }
}

#[cfg(unix)]
fn set_executable(dir: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    for name in desired_filenames() {
        let path = dir.join(&name);
        if path.is_file() {
            let mut perms = std::fs::metadata(&path)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_triple_resolves_for_current_platform() {
        // On any supported CI host this must succeed and name a known triple.
        let triple = host_triple().expect("supported host");
        assert!(
            triple.contains("windows") || triple.contains("darwin") || triple.contains("linux"),
            "unexpected triple: {triple}"
        );
    }

    #[test]
    fn desired_filenames_lists_all_three_binaries() {
        let names = desired_filenames();
        assert_eq!(names.len(), 3);
        assert!(names.iter().any(|n| n.starts_with("zccache-daemon")));
        assert!(names.iter().any(|n| n.starts_with("zccache-fp")));
    }

    #[test]
    fn managed_zccache_lives_under_mode_root_bin_not_cache_runtime() {
        let root = fbuild_paths::get_fbuild_root();
        let cache_root = fbuild_paths::get_cache_root();
        let dir = managed_dir();
        let managed_version_dir = format!("zccache-{MANAGED_ZCCACHE_VERSION}");

        assert!(dir.starts_with(root.join("bin")));
        assert!(!dir.starts_with(&cache_root));
        assert_eq!(
            dir.file_name().and_then(|name| name.to_str()),
            Some(managed_version_dir.as_str())
        );
        assert_eq!(managed_zccache_exe().parent(), Some(dir.as_path()));
    }

    #[test]
    fn sha256_for_asset_parses_dot_slash_entries() {
        let sums = "\
601f68d19f2bfdc0f277636851dc582989fbcb697c6754952416dd6a2a9b2adb  ./zccache-v1.12.0-x86_64-unknown-linux-musl.tar.gz
0fe54afd0c87e4d64a34b903107d9a4e1f4c5cf5274400482cbb64f45ea31c14  ./zccache-v1.12.0-x86_64-pc-windows-msvc.zip
";
        assert_eq!(
            sha256_for_asset(sums, "zccache-v1.12.0-x86_64-unknown-linux-musl.tar.gz").as_deref(),
            Some("601f68d19f2bfdc0f277636851dc582989fbcb697c6754952416dd6a2a9b2adb")
        );
        assert_eq!(
            sha256_for_asset(sums, "zccache-v1.12.0-x86_64-pc-windows-msvc.zip").as_deref(),
            Some("0fe54afd0c87e4d64a34b903107d9a4e1f4c5cf5274400482cbb64f45ea31c14")
        );
        assert!(sha256_for_asset(sums, "does-not-exist.tar.gz").is_none());
    }

    #[test]
    fn to_hex_lowercases_bytes() {
        assert_eq!(to_hex(&[0x00, 0x0f, 0xab, 0xff]), "000fabff");
    }

    #[test]
    fn match_desired_tolerates_exe_suffix() {
        let desired = vec!["zccache.exe".to_string(), "zccache-fp.exe".to_string()];
        assert_eq!(
            match_desired("zccache", &desired).map(String::as_str),
            Some("zccache.exe")
        );
        assert_eq!(
            match_desired("zccache.exe", &desired).map(String::as_str),
            Some("zccache.exe")
        );
        assert!(match_desired("README.md", &desired).is_none());
    }

    #[test]
    fn extract_tar_gz_pulls_binaries_by_basename_from_nested_dir() {
        let tar_gz = build_tar_gz(&[
            ("zccache-v1.12.0-host/README.md", b"readme"),
            ("zccache-v1.12.0-host/zccache", b"cli"),
            ("zccache-v1.12.0-host/zccache-daemon", b"daemon"),
            ("zccache-v1.12.0-host/zccache-fp", b"fp"),
        ]);

        let tmp = tempfile::tempdir().unwrap();
        extract_tar_gz(&tar_gz, tmp.path()).unwrap();

        for name in desired_filenames() {
            assert!(
                tmp.path().join(&name).is_file(),
                "expected extracted binary {name}"
            );
        }
        // Non-binary entries are skipped.
        assert!(!tmp.path().join("README.md").exists());
    }

    #[test]
    fn extract_tar_gz_errors_when_a_binary_is_missing() {
        let tar_gz = build_tar_gz(&[
            ("top/zccache", b"cli"),
            ("top/zccache-daemon", b"daemon"),
            // zccache-fp intentionally absent
        ]);
        let tmp = tempfile::tempdir().unwrap();
        let err = extract_tar_gz(&tar_gz, tmp.path()).expect_err("missing binary should fail");
        assert!(
            err.to_string().contains("zccache-fp"),
            "error must name the missing binary: {err}"
        );
    }

    #[test]
    fn install_lock_blocks_second_caller_until_released() {
        // What this test proves: holding the install lock makes a contending
        // acquire wait until the first holder releases.
        //
        // Earlier shape — `std::thread::sleep(50ms); drop(first); assert waited >= 40ms` —
        // was flaky under parallel-test contention: `std::thread::spawn` doesn't
        // guarantee the spawned thread is scheduled before the main thread's sleep
        // ends. If the waiter wasn't scheduled until after `drop(first)`, it would
        // acquire instantly and report `waited ≈ 0ms`, failing the deadline.
        //
        // New shape: use `JoinHandle::is_finished` to prove the waiter is *still*
        // blocked while we hold the lock — that's the actual invariant we care
        // about. The wall-clock elapsed assertion becomes a much softer "waiter
        // observed at least one polling sleep," which doesn't race with scheduler
        // latency.
        let tmp = tempfile::tempdir().unwrap();
        let lock_dir = tmp.path().join("zccache.lock");
        let first = acquire_install_lock_at(
            &lock_dir,
            Duration::from_secs(60),
            Duration::from_millis(10),
        )
        .expect("first lock");
        assert!(lock_dir.is_dir());

        let lock_for_thread = lock_dir.clone();
        let waiter = std::thread::spawn(move || {
            let started = Instant::now();
            let _second = acquire_install_lock_at(
                &lock_for_thread,
                Duration::from_secs(60),
                Duration::from_millis(10),
            )
            .expect("second lock");
            started.elapsed()
        });

        // Give the waiter a generous window to start polling. 100ms is far
        // longer than any reasonable scheduler latency, so on a healthy run
        // the waiter is definitely inside `acquire_install_lock_at` by now.
        std::thread::sleep(Duration::from_millis(100));

        // Load-bearing assertion: the waiter MUST still be blocked while we
        // hold the lock. If it isn't, the lock isn't actually contended.
        assert!(
            !waiter.is_finished(),
            "second lock acquire completed while the first lock was still held — \
             the install lock is not blocking contended callers"
        );

        drop(first);

        let waited = waiter.join().expect("waiter thread");
        // Soft sanity check: at least some elapsed time. Don't pin a tight
        // deadline — scheduler jitter on parallel test runs can hide between
        // `Instant::now()` and the first `create_dir` call.
        assert!(
            waited > Duration::from_millis(0),
            "waiter should have measured a non-zero acquire duration, got {waited:?}"
        );
    }

    #[test]
    fn install_lock_recovers_stale_lock_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let lock_dir = tmp.path().join("zccache.lock");
        std::fs::create_dir(&lock_dir).unwrap();

        let _guard =
            acquire_install_lock_at(&lock_dir, Duration::from_secs(0), Duration::from_millis(1))
                .expect("stale lock should be replaced");

        let owner = std::fs::read_to_string(lock_dir.join("owner.txt")).unwrap();
        assert!(owner.contains("version="));
    }

    #[test]
    fn cleanup_stale_install_staging_removes_only_old_zccache_temp_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let stale = tmp.path().join(".zccache-deadbeef.tmp");
        let fresh = tmp.path().join(".zccache-active.tmp");
        let unrelated = tmp.path().join(".other-tool.tmp");
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::create_dir_all(&fresh).unwrap();
        std::fs::create_dir_all(&unrelated).unwrap();
        std::fs::write(stale.join("zccache"), b"partial").unwrap();

        let old = filetime::FileTime::from_unix_time(1, 0);
        filetime::set_file_mtime(&stale, old).unwrap();
        filetime::set_file_mtime(&unrelated, old).unwrap();

        let removed = cleanup_stale_install_staging(tmp.path(), Duration::from_secs(60)).unwrap();

        assert_eq!(removed, 1);
        assert!(!stale.exists());
        assert!(fresh.exists());
        assert!(unrelated.exists());
    }

    #[test]
    fn cleanup_stale_install_staging_ignores_final_install_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let final_dir = tmp
            .path()
            .join(format!("zccache-{MANAGED_ZCCACHE_VERSION}"));
        std::fs::create_dir_all(&final_dir).unwrap();
        let old = filetime::FileTime::from_unix_time(1, 0);
        filetime::set_file_mtime(&final_dir, old).unwrap();

        let removed = cleanup_stale_install_staging(tmp.path(), Duration::from_secs(60)).unwrap();

        assert_eq!(removed, 0);
        assert!(final_dir.exists());
    }

    /// Real end-to-end download against GitHub Releases. Ignored by
    /// default (needs network); run manually with
    /// `soldr cargo test -p fbuild-build --lib managed_zccache -- --ignored`.
    #[test]
    #[ignore = "network: downloads the pinned zccache release from GitHub"]
    fn ensure_downloads_and_extracts_real_release() {
        let tmp = tempfile::tempdir().unwrap();
        // get_fbuild_root() reads USERPROFILE (Windows) / HOME (unix).
        let home_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        let prev = std::env::var_os(home_var);
        std::env::set_var(home_var, tmp.path());

        let result = download_and_install();
        // Capture the resolved dir while the home override is still active.
        let install_dir = managed_dir();

        // Restore before asserting so a failure doesn't leak the override.
        match prev {
            Some(v) => std::env::set_var(home_var, v),
            None => std::env::remove_var(home_var),
        }
        result.expect("real download should succeed");

        for name in desired_filenames() {
            assert!(
                install_dir.join(&name).is_file(),
                "expected downloaded binary {name} under {}",
                install_dir.display()
            );
        }
    }

    fn build_tar_gz(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut builder = tar::Builder::new(enc);
        for (path, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, path, *data).unwrap();
        }
        let enc = builder.into_inner().unwrap();
        enc.finish().unwrap()
    }
}
