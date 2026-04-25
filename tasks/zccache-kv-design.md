# zccache K/V — feature proposal (origin: FastLED/fbuild#205, Phase 4)

## Summary

Extend the **existing** `zccache` crate workspace and CLI with a small,
namespaced, blake3-keyed key/value store. **Not a new binary**, not a new
top-level crate — the K/V API lives next to `ArtifactStore` in the existing
`zccache-artifact` crate, reuses the existing `~/.zccache/index.redb`
database file (separate redb table), and surfaces through the existing
`zccache` CLI as new subcommands (`zccache kv get|put|ls|rm|clear`).

The motivation is fbuild's #205 Phase 4 — memoizing PlatformIO-LDF-style
library-selection results between builds. The data shape (`Selection`:
include closure + selected library names + compile/include sets) does not
fit the compile-action-shaped `ArtifactStore`. A general-purpose K/V is the
clean answer and useful beyond fbuild.

## Why fold into the existing crate, not split

- Avoids a new `zccache-kv` crate, a new bin, a new release artifact.
  Today `zccache-artifact` already owns redb. Adding a second redb table
  alongside `ARTIFACTS_TABLE` is one file and ~150 lines.
- Single binary surface for users: `zccache kv ...` parallels
  `zccache artifact ...`. No discoverability fragmentation.
- Single backup / nuke target: `~/.zccache/` purges everything.
- Versioned together: a zccache release advances both surfaces atomically.

## Public API

In `crates/zccache-artifact/src/kv.rs` (new file), exported from the
crate's `lib.rs`:

```rust
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key(pub [u8; 32]);

impl Key {
    pub fn from_hash(h: blake3::Hash) -> Self;
    pub fn as_bytes(&self) -> &[u8; 32];
    pub fn to_hex(&self) -> String;          // 64-char lowercase
    pub fn from_hex(hex: &str) -> Result<Self, KvError>;
}

#[derive(Debug, thiserror::Error)]
pub enum KvError {
    #[error("io: {0}")] Io(#[from] std::io::Error),
    #[error("redb: {0}")] Redb(String),
    #[error("namespace must be 1..=64 chars of [a-z0-9-]")] BadNamespace,
    #[error("key must be 32 bytes (64 hex chars)")] BadKey,
    #[error("corrupt entry for key {0}: {1}")] Corrupt(String, String),
    #[error("value too large: {0} bytes (max {1})")] TooLarge(usize, usize),
}
pub type KvResult<T> = std::result::Result<T, KvError>;

pub struct KvStore { /* private: holds &Database or owns one */ }

impl KvStore {
    /// Open under the canonical zccache root (`~/.zccache/` or
    /// `$ZCCACHE_DIR`). Reuses the same redb file as `ArtifactStore`.
    pub fn open_default() -> KvResult<Self>;
    /// Open at an explicit dir (test / ephemeral use).
    pub fn open<P: AsRef<Path>>(dir: P) -> KvResult<Self>;
    /// Share an already-open redb `Database` so the artifact and KV stores
    /// can co-exist without contending on Database creation.
    pub fn from_database(db: std::sync::Arc<redb::Database>) -> Self;

    /// Cache miss returns `Ok(None)`. `Err` only on backend or corruption.
    pub fn get(&self, namespace: &str, key: &Key) -> KvResult<Option<Vec<u8>>>;
    /// Last-writer-wins. Returns bytes written.
    pub fn put(&self, namespace: &str, key: &Key, value: &[u8]) -> KvResult<usize>;
    /// Idempotent (missing key is not an error).
    pub fn remove(&self, namespace: &str, key: &Key) -> KvResult<()>;
    /// Drop every entry under one namespace.
    pub fn clear_namespace(&self, namespace: &str) -> KvResult<()>;
    /// Iterator-by-collection (sorted by hex key) for `kv ls`.
    pub fn list_namespace(&self, namespace: &str) -> KvResult<Vec<(Key, u64)>>;

    /// Total bytes across every namespace.
    pub fn total_bytes(&self) -> KvResult<u64>;
    pub fn namespace_bytes(&self, namespace: &str) -> KvResult<u64>;
}
```

### Storage layout

```
~/.zccache/
  index.redb          # SHARED with ArtifactStore (separate redb table)
  kv/
    <namespace>/
      <64-hex>.bin    # raw value bytes for values > INLINE_THRESHOLD
```

- Values **≤ `INLINE_THRESHOLD = 4 KiB`** are stored inline in the redb
  table (low overhead, single fsync, no second open).
- Values **> 4 KiB** are spilled to disk under `kv/<namespace>/<hex>.bin`.
  The redb table holds a marker indicating spill + payload length + a
  blake3 of the file body (for corruption detection on read).
- Hard cap: `MAX_VALUE_BYTES = 64 MiB`. Over-cap → `Err(TooLarge)`.

### Redb table schema

```rust
const KV_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("kv");
```

Composite key encoding: `format!("{namespace}::{hex_key}")`. Namespaces
are validated `[a-z0-9-]` so `::` is unambiguous.

Row value (bincode-serialized):

```rust
struct KvRow {
    schema_version: u32,         // bump on layout change
    body: KvBody,                // Inline(Vec<u8>) or Spilled { len, blake3 }
}
```

### Namespace rules

- `[a-z0-9-]`, 1..=64 chars. Anything else → `KvError::BadNamespace`.
- Reserved namespaces (no enforcement, just convention):
  - `library-selection` — fbuild #205.
  - `compile-graph` — future use.
  - `_test` — anything beginning with `_test` is fair game for tests.

## CLI surface (in `zccache-cli`)

```
zccache kv get <namespace> <hex-key>            # writes value to stdout
zccache kv put <namespace> <hex-key> [--value-from <file>|--value-from-stdin]
zccache kv rm  <namespace> <hex-key>
zccache kv ls  <namespace>                      # one row per entry: <hex>  <bytes>
zccache kv clear <namespace>
zccache kv stats                                # total / per-namespace bytes
```

Exit codes match the rest of the CLI. `kv get` on a miss exits 2 with no
stdout (parseable from shell).

---

## Test plan — comprehensive + adversarial

All tests live in `crates/zccache-artifact/src/kv.rs` mod tests, plus a
small CLI integration test under `crates/zccache-cli/tests/kv.rs`. No
mocks; `tempfile::TempDir` per test.

### Functional

- **F1**: `put` then `get` returns the same bytes. Value sizes: 0, 1,
  100, 4 KiB - 1, 4 KiB, 4 KiB + 1, 100 KiB, 4 MiB.
- **F2**: `get` on missing key returns `Ok(None)`.
- **F3**: `put` overwrite returns the new value on subsequent `get`.
- **F4**: `remove` on a present key drops it; `get` is `None`. `remove`
  on missing key is `Ok(())` (idempotent).
- **F5**: `clear_namespace` drops every entry under that namespace and
  leaves entries in other namespaces untouched.
- **F6**: `list_namespace` returns hex-key-sorted `(Key, u64-len)` pairs.
- **F7**: `total_bytes` = sum of `namespace_bytes(ns)` over all namespaces.
- **F8**: Inline/spill threshold is byte-exact: 4096 stays inline, 4097
  spills. Spilled file size on disk equals `value.len()`.
- **F9**: Spilled file body blake3 matches the stored hash. Tampered
  spilled file → `KvError::Corrupt` on `get`.
- **F10**: `Key::from_hex` round-trips for valid input; rejects
  non-lowercase, non-64-char, non-hex.
- **F11**: Namespace validator: accepts `a`, `0`, `library-selection`;
  rejects `""`, `"A"`, `"name with space"`, `"a/b"`, `"日本語"`,
  65-char input.
- **F12**: Schema-version mismatch on read returns
  `KvError::Corrupt(_, "schema_version=…")` rather than silent garbage.

### Adversarial — concurrency

These run on Linux + macOS + Windows CI matrix. They MUST be
deterministic on every OS — no `sleep`-based timing, only
join-on-thread synchronization.

- **C1 — Same-key thundering herd**: 16 threads each call
  `put(ns, k, [thread_id; 1024])` 100 times. Final `get(ns, k)` returns
  one of the values (no torn read, no `None`, no error). Use
  `std::thread::scope` so the test owns the threads.
- **C2 — Distinct-key parallel writers**: 32 threads, each writing 100
  unique keys. After join, every key reads back. Ordering doesn't
  matter; correctness does.
- **C3 — Reader/writer race**: 8 reader threads spinning on
  `get(ns, k)` while 8 writer threads spam `put(ns, k, ...)`. Readers
  must observe only well-formed responses — `Ok(Some(_))` with bytes
  matching some written value, or `Ok(None)` (if the very first read
  beats the first write). Never `Err`.
- **C4 — Open-while-write**: thread A holds an open `KvStore`, thread B
  opens a second `KvStore` on the same dir and `put`s. Thread A's `get`
  sees the new value (redb's `WriteTransaction::commit` makes it visible
  to subsequent read txns).
- **C5 — `clear_namespace` during writes**: thread A `clear_namespace`s
  while thread B writes 1000 entries. After both join, the namespace is
  either empty or contains the post-clear writes — no partial state, no
  panic. (We don't promise atomicity vs. concurrent writes; we promise
  the store stays consistent.)

### Adversarial — durability / crash

- **D1 — Tempfile + rename atomicity (spill path)**: monkeypatch the
  spill writer to `panic!()` between `write_all` and `rename`. Reopen
  the store. Verify (a) the destination spill file does not exist, (b)
  the redb row for that key does not exist. Use `std::panic::catch_unwind`
  + a feature-gated `#[cfg(test)] static FAIL_AT: AtomicU8`.
- **D2 — Mid-commit redb crash sim**: write a row, drop the `KvStore`
  *without* committing (impossible in normal use — we always commit per
  call — but the test exercises the path by simulating an interrupted
  commit via the `redb` test API if available, or by killing a
  subprocess running the writer; whichever is easier). On reopen, the
  pre-crash state is intact and the partial write is absent.
- **D3 — Repeated open/close**: open the store, `put`, close, reopen,
  `get`, repeat 100 times in a tight loop. No file-handle leaks (file
  descriptor count stable on Linux/macOS via `/proc/self/fd`; on Windows
  via `GetProcessHandleCount`).

### Adversarial — platform compliance

- **P1 — Path-separator portability**: under `cfg(windows)`, `kv/`
  subdir uses backslashes via `Path::join`. Under `cfg(unix)` use
  forward slashes. Sanity-check `entry_path.parent() == kv_dir`.
- **P2 — Windows long path**: open the store at a deeply nested
  `TempDir` such that the spilled file path is > 260 chars. Confirm
  spill + readback succeed. Required path manipulation: prefix with
  `\\?\` (Rust's `std::fs` does this automatically on
  Windows ≥ 1.42 stdlib; verify with a test file written to the path).
  Skip on non-Windows.
- **P3 — Case-insensitive FS**: on macOS APFS (default case-insensitive)
  and Windows NTFS, two keys differing only in hex case must not
  collide. Easy: `Key::from_hex` lowercases; "DEADBEEF" and "deadbeef"
  parse to the same Key. Test by attempting to insert both and
  asserting the second is an overwrite, not a new entry. The hex
  representation we *write* to disk is always lowercase.
- **P4 — Symlinked store dir** (`cfg(unix)` only): create a TempDir,
  symlink it under another path, open the store via the symlink. Verify
  put/get round-trips and the symlink target receives the data.
- **P5 — Read-only directory**: chmod store dir to `0o555` (Unix) /
  attrib +R (Windows), `put` returns `Err(KvError::Io(_))`. Cleanup
  resets permissions.
- **P6 — UTF-8 namespace rejection**: `put("中文", ...)` → BadNamespace
  on every OS.
- **P7 — Large path on Linux**: write to a path at exactly NAME_MAX
  (255) for the spill file. Pass on Linux/Mac. (NAME_MAX irrelevant on
  Windows, where the limit is on the full path.)
- **P8 — fsync round-trip**: `put`, drop the `KvStore`, reopen, `get`
  returns the value. This is the most basic durability check; it must
  pass on every OS without any platform-specific code in the test.
- **P9 — Concurrent open-from-two-processes** (separate process test
  using `std::process::Command` so we exercise OS-level file locking,
  not just Rust's): spawn child A that holds the store open with a
  blocking write, spawn child B that opens the same dir. Verify either
  (a) B blocks until A finishes (redb file lock), or (b) B fails fast
  with a clear error. Do NOT silently corrupt.

### Adversarial — input

- **I1 — Empty namespace** → BadNamespace.
- **I2 — Namespace at limit (64 chars)** → ok.
- **I3 — Namespace at 65 chars** → BadNamespace.
- **I4 — Namespace with `::`** → BadNamespace (would collide with
  composite-key encoding).
- **I5 — Value at `MAX_VALUE_BYTES`** (64 MiB) → ok.
- **I6 — Value at `MAX_VALUE_BYTES + 1`** → TooLarge.
- **I7 — Same key reused across namespaces** → values are independent;
  asserting both round-trip.
- **I8 — `put` then crash before reading** → simulate via fresh
  `KvStore::open` after committing; `get` succeeds (redb durability).

### CLI integration

Under `crates/zccache-cli/tests/kv.rs`, using `assert_cmd`:

- `zccache kv put <ns> <hex> --value-from <file>` then
  `zccache kv get <ns> <hex>` round-trips bytes (binary safe — pipe
  through a temp file rather than capturing stdout as UTF-8).
- `zccache kv ls <ns>` lists exactly the keys put.
- `zccache kv rm` then `zccache kv get` exits 2 with empty stdout.
- `zccache kv clear <ns>` followed by `kv ls` is empty.
- `zccache kv stats` reports nonzero `total_bytes` after a `put`.

---

## Out of scope for the first release

- TTLs / eviction (cache is grow-forever; we'll add when usage demands).
- Compression of stored values.
- Async API. The store is sync; fbuild's resolver runs on Rayon, not
  Tokio.
- Cross-process advisory locking *beyond* what redb already provides.
- Arbitrary-precision keys. The 32-byte blake3 fingerprint is enough.

## Versioning

- Lands in the next minor zccache bump (currently 1.3.0 → 1.4.0).
- Breaking changes to the K/V API for one release while we shake out
  layout under #205 traffic; this is documented in CHANGELOG.
- The redb row format carries `schema_version: u32`; we do NOT promise
  forward compatibility within 1.x for the K/V table — opening a higher
  version with a lower binary is a hard error, not silent corruption.

## Coordination back to fbuild#205

When this lands and a release is cut, the fbuild side will:

1. Bump `zccache` workspace dep to the released version in
   `~/dev/fbuild3/Cargo.toml`.
2. Add `zccache-artifact = "<ver>"` as a Rust dep of
   `crates/fbuild-library-select/`.
3. Use `KvStore::open_default()` + namespace `"library-selection"` +
   the cache key composed in #205 Q9
   (blake3 of source hashes + canonical lib headers + search paths +
   toolchain triple + framework version + `SCANNER_VERSION` +
   `LDF_MODE_VERSION`).
4. Phase 4 integration tests (`#205 §4.1, §4.2`) exercise hit/miss,
   key stability, and corruption fallback on the real crate.

We post a status comment on FastLED/fbuild#205 with the released
zccache version, the bump PR link, and the integration test results.
