# Shared build engine

Compiler argv, build orchestration primitives, fingerprints and reusable caches.
`prebuilt_fp` hashes canonical resolved framework plans; `prebuilt` carries a
per-request daemon callback and retries a failed Hit once from source. The facade
in `fbuild-build` re-exports both modules. No prebuilt network/cache writer lives
in this crate.
