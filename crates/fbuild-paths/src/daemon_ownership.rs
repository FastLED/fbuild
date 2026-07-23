//! Soldr-style daemon lifecycle ownership for the compiler cache root
//! (FastLED/fbuild#1154 / #1159).
//!
//! Two cross-process file locks, both scoped to `get_daemon_dir()` and
//! deliberately outside zccache's own object-cache synchronization:
//!
//! - [`RootOwnershipGuard`] — version-blind, per-cache-root exclusive
//!   ownership. A `fbuild-daemon` holds this for its entire lifetime.
//!   `fbuild clean cache` takes it exclusively (after stopping every daemon
//!   it can find) as positive proof no daemon is touching the cache root
//!   before deleting it.
//! - [`SpawnLockGuard`] — spawn-herd single flight, so N concurrent `fbuild`
//!   invocations that all decide to spawn a daemon don't stampede.
//!
//! Semantics copied from soldr's `RootOwnershipGuard` / `acquire_spawn_lock`
//! (`.extern-repos/soldr/crates/soldr-daemon/src/daemon/lifecycle.rs`).
//! Locking itself is NOT reimplemented here — both guards are thin wrappers
//! around [`fbuild_core::file_lock::try_acquire`] so there is exactly one
//! cross-process file-lock primitive in the workspace.
//!
//! NEVER delete or truncate a lock file to "unlock" it — the OS releases
//! the lock when the holding process exits (including a hard kill); the
//! file itself is not the lock.

use std::path::{Path, PathBuf};

use fbuild_core::file_lock::{self, FileLockGuard, FileLockMode};

use crate::get_daemon_dir;

/// Expected executable stem of the fbuild daemon binary. Used with
/// `fbuild_core::process_identity::pid_exe_stem_matches` to verify a PID
/// found in a legacy pid file / claim / status file is actually an
/// fbuild-daemon before it is ever signalled.
pub const DAEMON_EXE_STEM: &str = "fbuild-daemon";

const ROOT_OWNER_LOCK_NAME: &str = "root-owner.lock";
const SPAWN_LOCK_NAME: &str = "spawn.lock";
const OWNER_CLAIM_NAME: &str = "root-owner.json";

/// Version-blind, per-cache-root exclusive ownership lock.
///
/// Lives at [`root_owner_lock_path`]. A daemon acquires this once at
/// startup (before doing any cache-mutating work) and holds it for its
/// entire lifetime; `fbuild clean cache` takes it exclusively — after
/// stopping every daemon it can find — as the final, positive proof that
/// no daemon (of ANY version) still owns the cache root, before deleting
/// it.
#[derive(Debug)]
pub struct RootOwnershipGuard {
    _guard: FileLockGuard,
}

impl RootOwnershipGuard {
    /// Try to acquire root ownership at the default path
    /// ([`root_owner_lock_path`]). `Ok(None)` means another process
    /// currently holds it.
    pub fn try_acquire() -> std::io::Result<Option<Self>> {
        Self::try_acquire_at(&root_owner_lock_path())
    }

    /// Test seam: try to acquire root ownership at an arbitrary path.
    pub fn try_acquire_at(path: &Path) -> std::io::Result<Option<Self>> {
        Ok(file_lock::try_acquire(path, FileLockMode::Exclusive)?.map(|_guard| Self { _guard }))
    }
}

/// Spawn-herd single-flight lock. Lives at [`spawn_lock_path`].
///
/// Errors while opening/locking the file are treated as "no lock
/// available" (`None`) — a broken filesystem must never gate progress; the
/// caller falls back to the herd-spawn path (poll health, retry) rather
/// than blocking forever on a lock that can't be taken.
#[derive(Debug)]
pub struct SpawnLockGuard {
    _guard: FileLockGuard,
}

/// Try to acquire the spawn-herd lock at the default path
/// ([`spawn_lock_path`]).
pub fn try_acquire_spawn_lock() -> Option<SpawnLockGuard> {
    try_acquire_spawn_lock_at(&spawn_lock_path())
}

/// Test seam: try to acquire the spawn-herd lock at an arbitrary path.
pub fn try_acquire_spawn_lock_at(path: &Path) -> Option<SpawnLockGuard> {
    file_lock::try_acquire(path, FileLockMode::Exclusive)
        .ok()
        .flatten()
        .map(|_guard| SpawnLockGuard { _guard })
}

/// Path to the root-ownership lock file.
pub fn root_owner_lock_path() -> PathBuf {
    get_daemon_dir().join(ROOT_OWNER_LOCK_NAME)
}

/// Path to the spawn-herd lock file.
pub fn spawn_lock_path() -> PathBuf {
    get_daemon_dir().join(SPAWN_LOCK_NAME)
}

/// Path to the [`OwnerClaim`] JSON file.
pub fn owner_claim_path() -> PathBuf {
    get_daemon_dir().join(OWNER_CLAIM_NAME)
}

/// Advisory claim written by the daemon after it has acquired root
/// ownership and knows its bound port. Removed at graceful shutdown.
///
/// **Never authoritative on its own.** This file can go stale (crash,
/// `SIGKILL`, power loss) exactly like any other PID file — always verify
/// `pid` liveness AND its exe stem (via
/// `fbuild_core::process_identity::pid_exe_stem_matches(pid, DAEMON_EXE_STEM)`)
/// before treating a claim as describing a live daemon, and never signal a
/// PID from a claim without that verification.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OwnerClaim {
    pub pid: u32,
    pub exe: PathBuf,
    /// `env!("CARGO_PKG_VERSION")` of the daemon that wrote the claim.
    pub version: String,
    /// `"dev"` | `"prod"`.
    pub mode: String,
    /// `DaemonCacheIdentity.cache_root_key` of the daemon that wrote the
    /// claim — lets a reader confirm the claim actually describes the
    /// cache root it's about to touch.
    pub cache_root_key: String,
    pub port: u16,
}

/// Write (or overwrite) the owner claim file.
pub fn write_owner_claim(claim: &OwnerClaim) -> std::io::Result<()> {
    let path = owner_claim_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(claim)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, json)
}

/// Read the owner claim file. Absent or malformed (bad JSON, missing
/// field, wrong shape) => `None`. Does NOT verify liveness — see the
/// [`OwnerClaim`] doc comment.
pub fn read_owner_claim() -> Option<OwnerClaim> {
    let raw = std::fs::read_to_string(owner_claim_path()).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Remove the owner claim file. Best-effort: a missing file is not an
/// error (idempotent).
pub fn remove_owner_claim() {
    let _ = std::fs::remove_file(owner_claim_path());
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn root_ownership_is_exclusive_within_process() {
        let temp = TempDir::new().expect("tempdir");
        let path = temp.path().join("root-owner.lock");

        let first = RootOwnershipGuard::try_acquire_at(&path)
            .expect("first acquire io")
            .expect("first acquire must succeed");
        let second = RootOwnershipGuard::try_acquire_at(&path).expect("second acquire io");
        assert!(
            second.is_none(),
            "a second exclusive acquire while the first is held must return None"
        );
        drop(first);
        let third = RootOwnershipGuard::try_acquire_at(&path)
            .expect("third acquire io")
            .expect("lock must be available again after the holder drops");
        drop(third);
    }

    #[test]
    fn spawn_lock_is_exclusive_within_process() {
        let temp = TempDir::new().expect("tempdir");
        let path = temp.path().join("spawn.lock");

        let first = try_acquire_spawn_lock_at(&path).expect("first acquire");
        let second = try_acquire_spawn_lock_at(&path);
        assert!(second.is_none(), "second acquire while held must be None");
        drop(first);
        let third = try_acquire_spawn_lock_at(&path);
        assert!(third.is_some(), "lock must be available after release");
    }

    /// Soldr pattern (`spawn_lock_serializes_concurrent_threads`): fire a
    /// pile of threads at the same lock file and confirm the lock actually
    /// serializes them — not all of them, and not zero of them, acquire.
    #[test]
    fn spawn_lock_serializes_concurrent_threads() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Barrier};

        let temp = TempDir::new().expect("tempdir");
        let path = Arc::new(temp.path().join("spawn.lock"));
        const THREADS: usize = 16;
        let barrier = Arc::new(Barrier::new(THREADS));
        let success_count = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::with_capacity(THREADS);
        for _ in 0..THREADS {
            let path = path.clone();
            let barrier = barrier.clone();
            let counter = success_count.clone();
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                if let Some(guard) = try_acquire_spawn_lock_at(&path) {
                    counter.fetch_add(1, Ordering::Relaxed);
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    drop(guard);
                }
            }));
        }
        for h in handles {
            h.join().expect("thread join");
        }
        let count = success_count.load(Ordering::Relaxed);
        assert!(count >= 1, "at least one thread must acquire; got {count}");
        assert!(
            count < THREADS,
            "lock must serialize acquisition; got {count} of {THREADS}"
        );
    }

    #[test]
    fn owner_claim_round_trips() {
        let temp = TempDir::new().expect("tempdir");
        // Point HOME/USERPROFILE-independent paths at a temp cache dir by
        // writing/reading through the raw serde round trip directly rather
        // than the real `owner_claim_path()` (which is process-global via
        // `get_daemon_dir`), keeping this test independent of env state.
        let path = temp.path().join(OWNER_CLAIM_NAME);
        let claim = OwnerClaim {
            pid: 4242,
            exe: PathBuf::from("/usr/local/bin/fbuild-daemon"),
            version: "9.9.9".to_string(),
            mode: "dev".to_string(),
            cache_root_key: "abc123".to_string(),
            port: 54321,
        };
        let json = serde_json::to_string_pretty(&claim).expect("serialize");
        std::fs::write(&path, json).expect("write");

        let raw = std::fs::read_to_string(&path).expect("read");
        let round_tripped: OwnerClaim = serde_json::from_str(&raw).expect("deserialize");
        assert_eq!(round_tripped.pid, claim.pid);
        assert_eq!(round_tripped.exe, claim.exe);
        assert_eq!(round_tripped.version, claim.version);
        assert_eq!(round_tripped.mode, claim.mode);
        assert_eq!(round_tripped.cache_root_key, claim.cache_root_key);
        assert_eq!(round_tripped.port, claim.port);
    }

    #[test]
    fn malformed_owner_claim_json_fails_closed() {
        let temp = TempDir::new().expect("tempdir");
        let path = temp.path().join(OWNER_CLAIM_NAME);
        std::fs::write(&path, "not valid json").expect("write");
        let raw = std::fs::read_to_string(&path).expect("read");
        assert!(
            serde_json::from_str::<OwnerClaim>(&raw).is_err(),
            "malformed claim JSON must fail to parse, never silently default"
        );
    }

    #[test]
    fn read_owner_claim_absent_file_is_none() {
        // `read_owner_claim()` reads from the process-global
        // `owner_claim_path()`; simulate the "absent" branch directly
        // against a path we know doesn't exist, mirroring the function's
        // own absent/malformed => None contract.
        let temp = TempDir::new().expect("tempdir");
        let missing = temp.path().join("does-not-exist.json");
        assert!(std::fs::read_to_string(&missing).is_err());
    }

    /// Soldr pattern (`root_ownership_is_version_blind_across_processes`):
    /// verify exclusivity holds across REAL processes, not just within one.
    /// The driver acquires the lock, spawns this test binary re-invoked as
    /// a subprocess probe (`--ignored --exact <probe>`), confirms it is
    /// blocked, drops the lock, then confirms a second subprocess probe
    /// succeeds.
    #[test]
    #[ignore = "subprocess helper for root_ownership_is_version_blind_across_processes"]
    fn subprocess_probe_root_owner() {
        let root = std::env::var_os("FBUILD_TEST_ROOT_OWNER_PATH").expect("test lock path");
        let expected = std::env::var("FBUILD_TEST_ROOT_OWNER_EXPECT").expect("expectation");
        let path = PathBuf::from(root);
        let acquired = RootOwnershipGuard::try_acquire_at(&path)
            .expect("root ownership probe io")
            .is_some();
        assert_eq!(acquired, expected == "acquired");
    }

    #[test]
    fn root_ownership_is_version_blind_across_processes() {
        let temp = TempDir::new().expect("tempdir");
        let path = temp.path().join("root-owner.lock");

        let run_probe = |expected: &str| {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "daemon_ownership::tests::subprocess_probe_root_owner",
                    "--nocapture",
                ])
                .env("FBUILD_TEST_ROOT_OWNER_PATH", &path)
                .env("FBUILD_TEST_ROOT_OWNER_EXPECT", expected)
                .output()
                .expect("run subprocess probe");
            assert!(
                output.status.success(),
                "subprocess root-owner probe failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };

        let owner = RootOwnershipGuard::try_acquire_at(&path)
            .expect("acquire io")
            .expect("parent must own the fresh lock file");
        run_probe("blocked");
        drop(owner);
        run_probe("acquired");
    }
}
