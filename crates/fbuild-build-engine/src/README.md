# Shared build engine

Compiler argv, build orchestration primitives, fingerprints and reusable caches.
`prebuilt_fp` hashes canonical resolved framework plans; `prebuilt` carries a
per-request daemon callback and retries a failed Hit once from source. The facade
in `fbuild-build` re-exports both modules. No prebuilt network/cache writer lives
in this crate.

Pilot framework compilation uses a stable framework CWD, absolute operands,
explicit file-prefix maps and a deterministic seed per logical source path.
Fingerprint paths use those same maps in GCC's last-match order; literal defines
are preserved. Output objects have logical TU ordinals in the transport archive
and are installed under the consumer's normal object filenames. Symlinked root
ambiguity and unsupported custom inputs fail closed. Shared BuildContext cleanup
invalidates previously delivered objects for direct callers and compile-many too.
