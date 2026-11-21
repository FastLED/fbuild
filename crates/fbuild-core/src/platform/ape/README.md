# `platform::ape` — fbuild APE loader adapter

`mod.rs` delegates detection and launch planning to the published
`running_process::ape` implementation. That implementation owns extraction,
validation, private atomic installation, nested APE launches, and the fork lock.

The adapter preserves `FBUILD_APE_LOADER`, `FBUILD_APE_CACHE_DIR`, `FBCACHE_DIR`,
`FBUILD_CACHE_DIR` and the cache root registered by fbuild's CLI and daemon.
Embedded zccache receives the equivalent canonical environment overrides;
its kernal-api dependency plans compiler launches directly with the original
compiler image, without a generated shim.

Tests here cover fbuild's settings, real contained/subprocess launches, and
48 concurrent first launches. Loader format and host-specific tests live in
running-process's concrete platform trees. The real compiler cold-miss/warm-hit
regression lives in `fbuild-build/tests/it/ape_toolchain.rs`.
