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
- **`fast_path.rs`** -- Shared fast-path check extracted from the
  ESP32 orchestrator. Takes a `FastPathInputs` (metadata hash,
  watches, required artifacts, optional zccache + stamp-cache) and
  returns `Some(FastPathHit)` when the caller can skip the pipeline
  entirely. Used by ESP32 + AVR today; Teensy / RP2040 / STM32 will
  follow.
