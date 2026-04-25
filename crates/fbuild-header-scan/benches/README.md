# benches

Criterion micro-benchmarks for `fbuild-header-scan`.

`scan_throughput.rs` measures `scan()` single-thread throughput (MB/s) over
three synthetic C++ fixtures: **tiny** (~64 B, per-call overhead),
**medium** (100 KB), and **large** (2 MB, stand-in for a Teensy-core-sized
translation unit). The fixtures exercise the scanner's adversary paths
(comments, string / raw-string literals containing fake `#include`s,
identifiers ending in `R` / `L`).

Per FastLED/fbuild#205 P-03 the aspirational threshold is **≥ 50 MB/s
single-thread**. This bench captures the baseline; it is not yet a CI
gate (Phase 7 will wire that up in a follow-up).

Run:

```bash
uv run soldr cargo bench -p fbuild-header-scan --bench scan_throughput
```
