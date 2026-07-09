//! Query, mutation, and lease-bookkeeping methods on [`CacheIndex`].
//!
//! Split out from `mod.rs` to keep individual files under the LOC gate.
//! All methods continue to live on the same `CacheIndex` type via
//! additional `impl` blocks — public API is unchanged.

use rusqlite::params;

use super::super::paths::{self, Kind};
use super::pid::is_pid_alive;
use super::{CacheEntry, CacheIndex};

impl CacheIndex {
    /// Look up an entry by kind, url, and version.
    pub fn lookup(
        &self,
        kind: Kind,
        url: &str,
        version: &str,
    ) -> rusqlite::Result<Option<CacheEntry>> {
        use rusqlite::OptionalExtension;
        let (_, hash) = paths::stem_and_hash(url);
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.query_row(
            "SELECT id, kind, url, stem, hash, version,
                    archive_path, archive_bytes, archive_sha256,
                    installed_path, installed_bytes, installed_at,
                    archived_at, last_used_at, use_count, pinned
             FROM entries
             WHERE kind = ?1 AND hash = ?2 AND version = ?3",
            params![kind.as_str(), hash, version],
            Self::row_to_entry,
        )
        .optional()
    }

    /// Record that an archive was downloaded.
    pub fn record_archive(
        &self,
        kind: Kind,
        url: &str,
        version: &str,
        archive_path: &str,
        archive_bytes: i64,
        archive_sha256: &str,
    ) -> rusqlite::Result<CacheEntry> {
        let (stem, hash) = paths::stem_and_hash(url);
        let now = Self::now_epoch();
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "INSERT INTO entries (kind, url, stem, hash, version,
                                  archive_path, archive_bytes, archive_sha256,
                                  archived_at, last_used_at, use_count, pinned)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0, 0)
             ON CONFLICT(kind, hash, version) DO UPDATE SET
                 archive_path = excluded.archive_path,
                 archive_bytes = excluded.archive_bytes,
                 archive_sha256 = excluded.archive_sha256,
                 archived_at = excluded.archived_at,
                 last_used_at = excluded.last_used_at",
            params![
                kind.as_str(),
                url,
                stem,
                hash,
                version,
                archive_path,
                archive_bytes,
                archive_sha256,
                now,
                now,
            ],
        )?;
        drop(conn);

        self.lookup(kind, url, version)
            .map(|opt| opt.expect("entry must exist after insert"))
    }

    /// Record that an archive was extracted/installed.
    pub fn record_install(
        &self,
        kind: Kind,
        url: &str,
        version: &str,
        installed_path: &str,
        installed_bytes: i64,
    ) -> rusqlite::Result<CacheEntry> {
        let (stem, hash) = paths::stem_and_hash(url);
        let now = Self::now_epoch();
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "INSERT INTO entries (kind, url, stem, hash, version,
                                  installed_path, installed_bytes, installed_at,
                                  last_used_at, use_count, pinned)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, 0)
             ON CONFLICT(kind, hash, version) DO UPDATE SET
                 installed_path = excluded.installed_path,
                 installed_bytes = excluded.installed_bytes,
                 installed_at = excluded.installed_at,
                 last_used_at = excluded.last_used_at",
            params![
                kind.as_str(),
                url,
                stem,
                hash,
                version,
                installed_path,
                installed_bytes,
                now,
                now,
            ],
        )?;
        drop(conn);

        self.lookup(kind, url, version)
            .map(|opt| opt.expect("entry must exist after insert"))
    }

    /// Bump LRU timestamp and use count for an entry.
    pub fn touch(&self, entry_id: i64) -> rusqlite::Result<()> {
        let now = Self::now_epoch();
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "UPDATE entries SET last_used_at = ?1, use_count = use_count + 1 WHERE id = ?2",
            params![now, entry_id],
        )?;
        Ok(())
    }

    /// Increment the pinned count for an entry (lease acquired).
    /// Uses a per-(PID, nonce) refcount so multiple `Lease` guards in the same
    /// process correctly track independent pins.
    pub fn pin(&self, entry_id: i64, holder_pid: u32, holder_nonce: u64) -> rusqlite::Result<()> {
        let now = Self::now_epoch();
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let updated = conn.execute(
            "UPDATE leases SET refcount = refcount + 1
             WHERE entry_id = ?1 AND holder_pid = ?2 AND holder_nonce = ?3",
            params![entry_id, holder_pid as i64, holder_nonce as i64],
        )?;
        if updated == 0 {
            conn.execute(
                "INSERT INTO leases (entry_id, holder_pid, holder_nonce, refcount, acquired_at)
                 VALUES (?1, ?2, ?3, 1, ?4)",
                params![entry_id, holder_pid as i64, holder_nonce as i64, now],
            )?;
        }
        conn.execute(
            "UPDATE entries SET pinned = (SELECT COALESCE(SUM(refcount), 0) FROM leases WHERE entry_id = ?1) WHERE id = ?1",
            params![entry_id],
        )?;
        Ok(())
    }

    /// Decrement the pinned count for an entry (lease released).
    /// Decrements refcount; removes the row when it reaches zero.
    pub fn unpin(&self, entry_id: i64, holder_pid: u32, holder_nonce: u64) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "UPDATE leases SET refcount = refcount - 1
             WHERE entry_id = ?1 AND holder_pid = ?2 AND holder_nonce = ?3",
            params![entry_id, holder_pid as i64, holder_nonce as i64],
        )?;
        conn.execute(
            "DELETE FROM leases
             WHERE entry_id = ?1 AND holder_pid = ?2 AND holder_nonce = ?3 AND refcount <= 0",
            params![entry_id, holder_pid as i64, holder_nonce as i64],
        )?;
        conn.execute(
            "UPDATE entries SET pinned = (SELECT COALESCE(SUM(refcount), 0) FROM leases WHERE entry_id = ?1) WHERE id = ?1",
            params![entry_id],
        )?;
        Ok(())
    }

    /// Get total bytes for all archives.
    pub fn total_archive_bytes(&self) -> rusqlite::Result<i64> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.query_row(
            "SELECT COALESCE(SUM(archive_bytes), 0) FROM entries WHERE archive_path IS NOT NULL",
            [],
            |row| row.get::<_, i64>(0),
        )
    }

    /// Get total bytes for all installed entries.
    pub fn total_installed_bytes(&self) -> rusqlite::Result<i64> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.query_row(
            "SELECT COALESCE(SUM(installed_bytes), 0) FROM entries WHERE installed_path IS NOT NULL",
            [],
            |row| row.get::<_, i64>(0),
        )
    }

    /// Get LRU installed entries (oldest first), skipping pinned.
    pub fn lru_installed_entries(&self, limit: usize) -> rusqlite::Result<Vec<CacheEntry>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT id, kind, url, stem, hash, version,
                    archive_path, archive_bytes, archive_sha256,
                    installed_path, installed_bytes, installed_at,
                    archived_at, last_used_at, use_count, pinned
             FROM entries
             WHERE installed_path IS NOT NULL AND pinned = 0
             ORDER BY last_used_at ASC
             LIMIT ?1",
        )?;

        let entries = stmt
            .query_map(params![limit as i64], Self::row_to_entry)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(entries)
    }

    /// Get LRU archive entries (oldest first), skipping pinned.
    pub fn lru_archive_entries(&self, limit: usize) -> rusqlite::Result<Vec<CacheEntry>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT id, kind, url, stem, hash, version,
                    archive_path, archive_bytes, archive_sha256,
                    installed_path, installed_bytes, installed_at,
                    archived_at, last_used_at, use_count, pinned
             FROM entries
             WHERE archive_path IS NOT NULL AND pinned = 0
             ORDER BY last_used_at ASC
             LIMIT ?1",
        )?;

        let entries = stmt
            .query_map(params![limit as i64], Self::row_to_entry)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(entries)
    }

    /// Null out the installed_path for an entry (after evicting its directory).
    pub fn clear_installed(&self, entry_id: i64) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "UPDATE entries SET installed_path = NULL, installed_bytes = NULL, installed_at = NULL WHERE id = ?1",
            params![entry_id],
        )?;
        Ok(())
    }

    /// Null out the archive_path for an entry (after evicting its archive).
    pub fn clear_archive(&self, entry_id: i64) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "UPDATE entries SET archive_path = NULL, archive_bytes = NULL, archive_sha256 = NULL, archived_at = NULL WHERE id = ?1",
            params![entry_id],
        )?;
        Ok(())
    }

    /// Delete an entry entirely (when both archive and installed are gone).
    pub fn delete_entry(&self, entry_id: i64) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute("DELETE FROM entries WHERE id = ?1", params![entry_id])?;
        Ok(())
    }

    /// Reap leases for dead PIDs.
    pub fn reap_dead_leases(&self) -> rusqlite::Result<usize> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare("SELECT DISTINCT holder_pid FROM leases")?;
        let pids: Vec<i64> = stmt
            .query_map([], |row| row.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);

        let mut reaped = 0;
        for pid in pids {
            if !is_pid_alive(pid as u32) {
                conn.execute("DELETE FROM leases WHERE holder_pid = ?1", params![pid])?;
                reaped += 1;
            }
        }

        // Refresh pinned counts for all affected entries — use SUM(refcount)
        // to match pin()/unpin() which also use SUM(refcount), not COUNT(*).
        if reaped > 0 {
            conn.execute_batch(
                "UPDATE entries SET pinned = (SELECT COALESCE(SUM(refcount), 0) FROM leases WHERE leases.entry_id = entries.id)",
            )?;
        }

        Ok(reaped)
    }

    /// Get all entries (for reconciliation).
    pub fn all_entries(&self) -> rusqlite::Result<Vec<CacheEntry>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT id, kind, url, stem, hash, version,
                    archive_path, archive_bytes, archive_sha256,
                    installed_path, installed_bytes, installed_at,
                    archived_at, last_used_at, use_count, pinned
             FROM entries",
        )?;

        let entries = stmt
            .query_map([], Self::row_to_entry)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(entries)
    }

    /// Look up the most recently used entry for a kind+url pair (any version).
    /// Returns the entry with the highest `last_used_at` that has an installed path.
    pub fn lookup_latest(&self, kind: Kind, url: &str) -> rusqlite::Result<Option<CacheEntry>> {
        use rusqlite::OptionalExtension;
        let (_, hash) = paths::stem_and_hash(url);
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.query_row(
            "SELECT id, kind, url, stem, hash, version,
                    archive_path, archive_bytes, archive_sha256,
                    installed_path, installed_bytes, installed_at,
                    archived_at, last_used_at, use_count, pinned
             FROM entries
             WHERE kind = ?1 AND hash = ?2 AND installed_path IS NOT NULL
             ORDER BY last_used_at DESC
             LIMIT 1",
            params![kind.as_str(), hash],
            Self::row_to_entry,
        )
        .optional()
    }

    /// Get total entry count.
    pub fn entry_count(&self) -> rusqlite::Result<i64> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.query_row("SELECT COUNT(*) FROM entries", [], |row| {
            row.get::<_, i64>(0)
        })
    }

    pub(super) fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<CacheEntry> {
        let kind_str: String = row.get(1)?;
        let kind: Kind = kind_str.parse().map_err(|e: String| {
            rusqlite::Error::FromSqlConversionFailure(
                1,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
            )
        })?;
        Ok(CacheEntry {
            id: row.get(0)?,
            kind,
            url: row.get(2)?,
            stem: row.get(3)?,
            hash: row.get(4)?,
            version: row.get(5)?,
            archive_path: row.get(6)?,
            archive_bytes: row.get(7)?,
            archive_sha256: row.get(8)?,
            installed_path: row.get(9)?,
            installed_bytes: row.get(10)?,
            installed_at: row.get(11)?,
            archived_at: row.get(12)?,
            last_used_at: row.get(13)?,
            use_count: row.get(14)?,
            pinned: row.get(15)?,
        })
    }
}
