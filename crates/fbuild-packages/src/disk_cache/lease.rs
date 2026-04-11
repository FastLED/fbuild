//! RAII lease guard that pins cache entries during builds.
//!
//! Holding a `Lease` prevents the GC from evicting the associated entry.
//! The lease is released on drop. Multiple leases from the same process
//! correctly maintain independent refcounts via a per-process nonce.

use super::index::CacheIndex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Per-process nonce, initialized once at startup to a value derived from
/// the process start time. Guards against PID reuse: a recycled PID will
/// have a different nonce and won't collide with stale lease rows.
fn process_nonce() -> u64 {
    static NONCE: AtomicU64 = AtomicU64::new(0);
    let val = NONCE.load(Ordering::Relaxed);
    if val != 0 {
        return val;
    }
    // Use current time as a nonce — unique enough to distinguish PID reuse.
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    // Race is benign: worst case two threads compute different nonces,
    // one wins, and both use the winner's value on subsequent calls.
    let nonce = nonce.max(1); // ensure non-zero
    NONCE.store(nonce, Ordering::Relaxed);
    nonce
}

/// RAII guard that pins a cache entry. Released on drop.
pub struct Lease {
    index: Arc<CacheIndex>,
    entry_id: i64,
    holder_pid: u32,
    holder_nonce: u64,
}

impl Lease {
    /// Acquire a lease for the given entry.
    pub fn acquire(index: Arc<CacheIndex>, entry_id: i64) -> rusqlite::Result<Self> {
        let pid = std::process::id();
        let nonce = process_nonce();
        index.pin(entry_id, pid, nonce)?;
        Ok(Self {
            index,
            entry_id,
            holder_pid: pid,
            holder_nonce: nonce,
        })
    }

    /// The entry ID this lease protects.
    pub fn entry_id(&self) -> i64 {
        self.entry_id
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if let Err(e) = self
            .index
            .unpin(self.entry_id, self.holder_pid, self.holder_nonce)
        {
            tracing::warn!(
                "failed to unpin lease entry_id={} holder_pid={}: {}",
                self.entry_id,
                self.holder_pid,
                e
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lease_pins_and_unpins_on_drop() {
        let idx = Arc::new(CacheIndex::open_in_memory().unwrap());
        let entry = idx
            .record_archive(
                super::super::paths::Kind::Packages,
                "https://example.com/a",
                "1.0",
                "path",
                100,
                "sha",
            )
            .unwrap();

        // Before lease
        let e = idx
            .lookup(
                super::super::paths::Kind::Packages,
                "https://example.com/a",
                "1.0",
            )
            .unwrap()
            .unwrap();
        assert_eq!(e.pinned, 0);

        // Acquire lease
        {
            let _lease = Lease::acquire(Arc::clone(&idx), entry.id).unwrap();
            let e = idx
                .lookup(
                    super::super::paths::Kind::Packages,
                    "https://example.com/a",
                    "1.0",
                )
                .unwrap()
                .unwrap();
            assert_eq!(e.pinned, 1);
        }

        // After drop
        let e = idx
            .lookup(
                super::super::paths::Kind::Packages,
                "https://example.com/a",
                "1.0",
            )
            .unwrap()
            .unwrap();
        assert_eq!(e.pinned, 0);
    }

    #[test]
    fn test_lease_blocks_lru_eviction() {
        let idx = Arc::new(CacheIndex::open_in_memory().unwrap());

        // Create installed entry
        let entry = idx
            .record_install(
                super::super::paths::Kind::Toolchains,
                "https://example.com/tool",
                "1.0",
                "installed/tool",
                5000,
            )
            .unwrap();

        // Acquire lease
        let _lease = Lease::acquire(Arc::clone(&idx), entry.id).unwrap();

        // LRU query should skip this pinned entry
        let lru = idx.lru_installed_entries(10).unwrap();
        assert!(lru.is_empty(), "leased entry should not appear in LRU list");
    }

    #[test]
    fn test_multiple_leases_same_pid_independent_refcount() {
        let idx = Arc::new(CacheIndex::open_in_memory().unwrap());
        let entry = idx
            .record_install(
                super::super::paths::Kind::Packages,
                "https://example.com/multi",
                "1.0",
                "path_multi",
                1000,
            )
            .unwrap();

        // Acquire two leases for the same entry from the same process
        let lease1 = Lease::acquire(Arc::clone(&idx), entry.id).unwrap();
        let lease2 = Lease::acquire(Arc::clone(&idx), entry.id).unwrap();

        let e = idx
            .lookup(
                super::super::paths::Kind::Packages,
                "https://example.com/multi",
                "1.0",
            )
            .unwrap()
            .unwrap();
        assert_eq!(e.pinned, 2, "two leases should give pinned=2");

        // Drop one lease — should still be pinned with count=1
        drop(lease1);
        let e = idx
            .lookup(
                super::super::paths::Kind::Packages,
                "https://example.com/multi",
                "1.0",
            )
            .unwrap()
            .unwrap();
        assert_eq!(e.pinned, 1, "one lease dropped, pinned should be 1");

        // Drop the second — fully unpinned
        drop(lease2);
        let e = idx
            .lookup(
                super::super::paths::Kind::Packages,
                "https://example.com/multi",
                "1.0",
            )
            .unwrap()
            .unwrap();
        assert_eq!(e.pinned, 0, "both leases dropped, pinned should be 0");
    }
}
