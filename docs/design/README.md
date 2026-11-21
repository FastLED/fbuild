# Design decision documents

Point-in-time decision records for features that span several crates and need
their open questions pinned down before implementation. Unlike
[`../DESIGN_DECISIONS.md`](../DESIGN_DECISIONS.md) (ADR-style, repo-wide,
durable) these are *per-feature* and are expected to be superseded by the
architecture doc for the subsystem once the feature lands.

## Contents

- **`artifact-serving-decisions.md`** — prebuilt per-chip framework-library
  slices served from object storage (FastLED/fbuild#1890): CLI opt-out flag,
  bucket key scheme, manifest freshness, streaming/resume/GC, mirror failover
  ladder, benchmark metrics, and the required test matrix. §8 (2026-11-21)
  records the decisions for exact-fingerprint framework *core-object* serving
  (FastLED/fbuild#1882): per-component identities instead of a tree hash, one
  opt-out shared with the slices, and why pilot PR FastLED/fbuild#1883 was
  closed unmerged.
- **`bare-idf-component-builds.md`** — decision record for FastLED/fbuild#1761:
  fbuild honors genuine bare-IDF / CMake-component projects through an opt-in
  (`board_build.build_system = espidf`) that delegates to a pinned ESP-IDF
  release. Contains a measured `idf.py` spike (v5.5.1, esp32s3), the three
  option costs, the measured cold-only caching decision, the
  false-coverage proof gate, the delivery map for follow-ups #1970-#1974, and
  the 2026-11-21 decision to close the native adapter (#1878) and widen the
  delegated path to the RISC-V targets instead.
