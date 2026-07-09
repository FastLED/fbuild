# fbuild-build-arm

Per-platform build orchestrators extracted from `fbuild-build` for compile
parallelism (FastLED/fbuild#1008). Depends only on `fbuild-build-engine`;
compiles in parallel with the sibling platform crates. Re-exported by the
`fbuild-build` facade at unchanged paths.
