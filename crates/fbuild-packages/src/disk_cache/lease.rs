//! RAII lease guard that pins cache entries during builds.
//!
//! Holding a `Lease` prevents the GC from evicting the associated entry.
//! The lease is released on drop.

use super::index::CacheIndex;
use std::sync::Arc;

/// RAII guard that pins a cache entry. Released on drop.
pub struct Lease {
    index: Arc<CacheIndex>,
    entry_id: i64,
    holder_pid: u32,
}

impl Lease {
    /// Acquire a lease for the given entry.
    pub fn acquire(index: Arc<CacheIndex>, entry_id: i64) -> rusqlite::Result<Self> {
        let pid = std::process::id();
        index.pin(entry_id, pid)?;
        Ok(Self {
            index,
            entry_id,
            holder_pid: pid,
        })
    }

    /// The entry ID this lease protects.
    pub fn entry_id(&self) -> i64 {
        self.entry_id
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let _ = self.index.unpin(self.entry_id, self.holder_pid);
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
}
