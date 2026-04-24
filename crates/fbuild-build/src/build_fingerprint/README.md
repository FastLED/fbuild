# `build_fingerprint` module

Persisted per-build metadata (hashes, stamps, size info) plus the
warm-build fast-path that lets orchestrators skip recompilation when
nothing relevant has changed.

## Contents

- **`mod.rs`** -- Core types (`PersistedBuildFingerprint`,
  `FileStamp`, `BinArtifactCache`, `SizeArtifactCache`), stamping
  primitives (`hash_watch_set`, `hash_watch_set_stamps`,
  `hash_watch_set_stamps_cached`), and the `WatchSetStampCache` trait
  the daemon implements for cross-invocation memoisation.
- **`fast_path.rs`** -- Shared warm-build cache contract. Orchestrators
  declare required outputs through `FastPathContract`, then use
  `fast_path_check` for reuse decisions and
  `persist_fast_path_success` to write `build_fingerprint.json` and
  mark zccache watch roots. Used by ESP32, AVR, Teensy, RP2040, SAM,
  NRF52, and Renesas.
