# enforce_platform_boundary

Pre-expansion Dylint for FastLED/fbuild#1308. It rejects host selectors,
compile-time host facts, native API paths, and private implementation names
outside `fbuild_core::platform`. Existing occurrences are admitted only by the
exact transitional `src/baseline.txt`; migration PRs delete rows.
