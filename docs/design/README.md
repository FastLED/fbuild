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
  ladder, benchmark metrics, and the required test matrix.
- **`bare-idf-component-builds.md`** — research note for FastLED/fbuild#1761:
  whether fbuild should honor genuine bare-IDF / CMake-component projects.
  Contains a measured `idf.py` spike (ESP-IDF v5.5.1, esp32s3), the three
  option costs, the false-coverage metadata guard, and the proposed follow-ups.
