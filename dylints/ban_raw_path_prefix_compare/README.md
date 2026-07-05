# `ban_raw_path_prefix_compare`

Bans raw `Path::starts_with` and `Path::strip_prefix` (on
`std::path::Path`, and therefore `PathBuf` via deref) in fbuild
production code — files under `crates/*/src/`.

## Why

`Path::starts_with` / `strip_prefix` compare path **components as
written**. Two spellings of the same location do not match: a
canonicalized dir (Windows `\\?\` prefix stripped, symlinks resolved,
macOS `/private/var/…`) versus a raw dir, a trailing slash, or a case
difference on case-insensitive filesystems. When one side is a cache
root or project directory and the other is a path pulled from a compiler
flag, the comparison silently mismatches — and a cross-project cache key
then encodes the project directory, so the global cache never hits.

That is exactly the bug behind FastLED/fbuild#952: a raw
`include_dir.starts_with(project_dir)` failed to strip project include
dirs from a signature because the spellings differed, giving every
project directory a distinct key.

## Use instead

Normalize both sides through the shared factory first, then compare:

```rust
use fbuild_core::path::normalize_for_key;
let dir_key = normalize_for_key(project_dir);
if normalize_for_key(candidate).starts_with(&format!("{dir_key}/")) { /* under */ }
```

or compare `fbuild_core::path::NormalizedPath` values. See
`agents/docs/path-conventions.md`.

## Allowlist

Legitimate raw comparisons — both sides already same-normalized, or the
result does not feed a cache key — are exempted file-by-file in
`src/allowlist.txt` with a one-line reason. Files outside
`crates/*/src/` are out of scope by design.

## Toolchain

Like the other fbuild dylints, this crate pins its own nightly in
`rust-toolchain.toml` (the rustc internal API moves fast) while the
workspace stays on stable. CI runs it via `cargo dylint --all` in
`.github/workflows/dylint.yml`.
