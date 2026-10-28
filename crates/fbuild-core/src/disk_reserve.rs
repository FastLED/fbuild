//! `ENOSPC` delete-to-recover emergency reserve (FastLED/fbuild#1750 item 6).
//!
//! The pre-flight check (`fbuild_build_engine::disk_preflight`) refuses a build
//! that starts on a full volume, and [`crate::disk_full`] translates a full
//! volume at a tool boundary. Neither helps when the volume fills *during* a
//! build and the failure path itself still has to write — a build log, a
//! partially written image, a daemon status file — onto the same full volume.
//!
//! Following the sibling `running-process` daemon (#390), fbuild commits a
//! small sacrificial file in the cache root before a build starts. When a
//! write later reports a full volume, the file is deleted: those blocks are
//! immediately available, so the error path can finish its bookkeeping and the
//! next attempt has a margin. It is re-armed at the start of the next build, a
//! missing file is tolerated, and an arm that cannot allocate (often because
//! the volume is already full) logs and degrades instead of failing a build
//! that might still work.
//!
//! This is the last resort, not a budget: 32 MiB does not make a large build
//! fit. Reclaiming real space is `fbuild cache reclaim` (#1750 item 3), which
//! stays explicit and never runs mid-build (#1341).

use std::io::{self, Write};
use std::path::Path;
use std::sync::Mutex;

use tracing::{debug, error, warn};

use crate::path::NormalizedPath;

/// Bytes held back for the failure path. Small enough to be irrelevant to a
/// multi-GiB ESP32 build, large enough for a log, a status file and an image
/// header.
pub const EMERGENCY_RESERVE_BYTES: u64 = 32 * 1024 * 1024;

/// Leaf name of the reserve file, placed directly in a build's cache root. A
/// bare name, not a hand-spelled project layout: the cache root already decides
/// where it lives (`ban_raw_fbuild_path`).
pub const EMERGENCY_RESERVE_FILE_NAME: &str = "emergency-reserve.bin";

/// Lifecycle state of a reserve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReserveState {
    /// The file exists at its full size and can still be released.
    Armed,
    /// The file was deleted in response to a full volume.
    Released,
    /// Pre-allocation failed (often because the volume was already full), so
    /// this build runs without the safety margin.
    Degraded,
}

impl ReserveState {
    /// Stable lowercase label for logs and status output.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Armed => "armed",
            Self::Released => "released",
            Self::Degraded => "degraded",
        }
    }
}

/// A pre-allocated disk reserve with delete-to-recover semantics.
#[derive(Debug)]
pub struct EmergencyReserve {
    path: NormalizedPath,
    size: u64,
    state: Mutex<ReserveState>,
}

impl EmergencyReserve {
    /// Create (or recreate) the reserve inside `dir` at [`EMERGENCY_RESERVE_BYTES`].
    /// Never fails: pre-allocation errors degrade instead.
    pub fn initialize_in(dir: &Path) -> Self {
        Self::initialize_at(NormalizedPath::from(dir.join(EMERGENCY_RESERVE_FILE_NAME)))
    }

    /// [`Self::initialize_in`] with an explicit path and size.
    ///
    /// An existing file is recreated from scratch, so a partial file left by a
    /// crashed run is replaced by a full-size reserve rather than trusted.
    pub fn initialize_at(path: NormalizedPath) -> Self {
        Self::initialize_at_size(path, EMERGENCY_RESERVE_BYTES)
    }

    fn initialize_at_size(path: NormalizedPath, size: u64) -> Self {
        let state = match preallocate(path.as_path(), size) {
            Ok(()) => {
                debug!(
                    "emergency reserve armed: {} ({} bytes)",
                    path.display_slash(),
                    size
                );
                ReserveState::Armed
            }
            Err(err) => {
                warn!(
                    "emergency reserve pre-allocation failed at {} ({err}); \
                     continuing without ENOSPC headroom",
                    path.display_slash()
                );
                ReserveState::Degraded
            }
        };
        Self {
            path,
            size,
            state: Mutex::new(state),
        }
    }

    /// Reserve file location.
    pub fn path(&self) -> &Path {
        self.path.as_path()
    }

    /// Configured reserve size in bytes.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Current lifecycle state.
    pub fn state(&self) -> ReserveState {
        *self.lock()
    }

    /// Delete the reserve to regain disk headroom. Idempotent: repeated calls
    /// and an already-missing file both succeed. Returns `true` only when this
    /// call performed the release.
    pub fn release(&self, reason: &str) -> bool {
        let mut state = self.lock();
        if *state == ReserveState::Released {
            return false;
        }
        match std::fs::remove_file(self.path.as_path()) {
            Ok(()) => {
                error!(
                    "ENOSPC recovery: released emergency reserve {} ({} bytes) — {reason}",
                    self.path.display_slash(),
                    self.size
                );
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                warn!(
                    "ENOSPC recovery: emergency reserve {} already missing — {reason}",
                    self.path.display_slash()
                );
            }
            Err(err) => {
                error!(
                    "ENOSPC recovery: failed to delete emergency reserve {} ({err}) — {reason}",
                    self.path.display_slash()
                );
                return false;
            }
        }
        *state = ReserveState::Released;
        true
    }

    /// Release when `error` says the volume is full. Returns `true` when this
    /// error caused a release.
    pub fn release_if_enospc(&self, error: &io::Error, context: &str) -> bool {
        if !crate::disk_full::is_enospc_io(error) {
            return false;
        }
        self.release(context)
    }

    /// Release when a tool's output only survives as text (a subprocess's
    /// stderr). Returns `true` when this message caused a release.
    pub fn release_if_full_message(&self, message: &str, context: &str) -> bool {
        if crate::disk_full::enospc_signature(message).is_none() {
            return false;
        }
        self.release(context)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ReserveState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// The process-wide reserve, armed by whichever build path starts first.
///
/// A global rather than a threaded parameter because the release has to happen
/// at *every* tool boundary that can report a full volume — the archiver in this
/// crate, the package downloader, the ESP32 linker — and none of them owns the
/// build's disk bookkeeping.
static PROCESS_RESERVE: Mutex<Option<EmergencyReserve>> = Mutex::new(None);

/// Arm the process-wide reserve inside `dir` (a build's cache root), replacing
/// one that a previous build released. Returns `true` when this call armed it,
/// `false` when a reserve is already armed or could only degrade.
pub fn arm_in(dir: &Path) -> bool {
    let mut slot = lock_process_reserve();
    let already_armed = slot
        .as_ref()
        .is_some_and(|existing| existing.state() != ReserveState::Released);
    if already_armed {
        return false;
    }
    let reserve = EmergencyReserve::initialize_in(dir);
    let armed = reserve.state() == ReserveState::Armed;
    *slot = Some(reserve);
    armed
}

/// Current process-wide state, or `None` when no build armed a reserve.
pub fn process_state() -> Option<ReserveState> {
    lock_process_reserve().as_ref().map(EmergencyReserve::state)
}

/// Release the process-wide reserve because `context` hit a full volume.
/// Returns the bytes freed, or `None` when there was nothing to release.
pub fn release_for(context: &str) -> Option<u64> {
    let slot = lock_process_reserve();
    let reserve = slot.as_ref()?;
    if !reserve.release(context) {
        return None;
    }
    Some(reserve.size())
}

/// Release the process-wide reserve when `error` says the volume is full.
pub fn release_if_enospc(error: &io::Error, context: &str) -> Option<u64> {
    if !crate::disk_full::is_enospc_io(error) {
        return None;
    }
    release_for(context)
}

/// The line a disk-space failure gains when a reserve was released by it, or
/// empty when none was armed. Appended after [`crate::disk_full::DISK_FULL_HINT`]
/// so the actionable advice still comes first.
pub fn release_note(context: &str) -> String {
    match release_for(context) {
        Some(bytes) => format!(
            "\n  released the {} emergency reserve so this failure could still be recorded; \
             the next build re-arms it",
            mib(bytes)
        ),
        None => String::new(),
    }
}

fn mib(bytes: u64) -> String {
    format!("{} MiB", bytes / (1024 * 1024))
}

fn lock_process_reserve() -> std::sync::MutexGuard<'static, Option<EmergencyReserve>> {
    PROCESS_RESERVE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Commit `size` real bytes at `path`.
///
/// `set_len` alone produces a sparse file on most filesystems and would reserve
/// no blocks at all, so the file is written through in bounded chunks. A partial
/// file is removed on failure: a half-size reserve is a lie about the margin.
fn preallocate(path: &Path, size: u64) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::File::create(path)?;
    file.set_len(size)?;
    const CHUNK: usize = 1024 * 1024;
    let chunk = vec![0u8; CHUNK];
    let mut remaining = size;
    while remaining > 0 {
        let take = remaining.min(CHUNK as u64) as usize;
        if let Err(err) = file.write_all(&chunk[..take]) {
            drop(file);
            let _ = std::fs::remove_file(path);
            return Err(err);
        }
        remaining -= take as u64;
    }
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
#[path = "disk_reserve_tests.rs"]
mod tests;
