# `ban_std_pathbuf`

This lint bans `std::path::PathBuf` in workspace code and directs developers to
`fbuild_core::path::NormalizedPath` instead.

## Why

Raw `PathBuf` values don't carry fbuild's normalization invariant, which has
caused Windows-only cache-key and watcher mismatches in the past
(FastLED/fbuild#436, #437, #282). `NormalizedPath` does the normalization once
at construction time and caches the hash key, so two paths that point at the
same file always compare equal and hash equal regardless of how the caller
spelled them.

## Rollout

The lint is ON and denies new `PathBuf` usage by default. The repository still
has legacy `PathBuf` call sites; those files are listed in `src/allowlist.txt`.
New files are denied. Remove files from the allowlist as migrations land — the
target state is zero entries.

## See also

- FastLED/fbuild#826 — the dylint sweep tracking issue
- FastLED/fbuild#436 / #437 / #282 — the Windows path-normalization bugs that
  motivated `NormalizedPath`
