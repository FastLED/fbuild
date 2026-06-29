# `ban_unrooted_tempdir`

This lint bans calls that create scratch directories or files under the OS
temp dir (`$TMPDIR` / `%TEMP%`) and steers production code toward paths
rooted under fbuild's user-visible cache tree (`fbuild_paths::get_cache_root()`
/ `get_fbuild_root()`).

## Why

Every byte fbuild writes should live under one ground-truth directory the
user can inspect, override, or clean. Scratch dirs scattered across
`$TMPDIR` are invisible to `fbuild`'s own cleanup commands and survive
process death on Windows for hours. On Windows, a temp dir on a different
volume from the destination also breaks the atomic-rename invariant that
`tempfile::NamedTempFile::persist` relies on, which silently degrades to a
copy+delete and races with concurrent readers.

The banned APIs are:

| Banned call                        | Replacement                                                                       |
| ---------------------------------- | --------------------------------------------------------------------------------- |
| `std::env::temp_dir()`             | `fbuild_paths::get_cache_root()` (or a named subdir under it)                     |
| `tempfile::tempdir()`              | `tempfile::tempdir_in(<path under fbuild cache root>)`                            |
| `tempfile::TempDir::new()`         | `tempfile::TempDir::new_in(<path under fbuild cache root>)`                       |
| `tempfile::NamedTempFile::new()`   | `tempfile::NamedTempFile::new_in(<same dir as the rename target>)` (atomic write) |

The `*_in(...)` variants take an explicit path and are always allowed.

## Rollout

The lint is ON and denies new unrooted temp-dir usage by default. The
repository has legacy call sites that pre-date this lint; those files are
listed in `src/allowlist.txt`. Files under `tests/` and `benches/` are
blanket-allowed (they don't ship to users). The target state is zero
allowlist entries — remove files as their temp-dir usage is migrated.

## See also

- FastLED/fbuild#826 — the dylint sweep tracking issue
