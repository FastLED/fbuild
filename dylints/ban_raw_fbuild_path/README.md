# `ban_raw_fbuild_path`

This lint bans string literals that spell the `.fbuild` directory
segment by hand and directs developers at `fbuild_paths` — the crate
that declares itself the single source of truth for all `.fbuild`
paths.

## Why

`crates/fbuild-paths/src/lib.rs:3` says it owns every `.fbuild` path,
but the layout underneath it is not a fixed string:

- the env segment **auto-collapses** when a project has a single
  environment (`<project>/.fbuild/build/release/`, no env dir),
- `FBUILD_BUILD_DIR` **overrides the root wholesale**,
- PlatformIO-style projects nest the tree under
  `.build/pio/<env>/.fbuild/build/<env>/<profile>/`.

A hardcoded `dir.join(".fbuild/build/uno/release")` encodes exactly one
of those shapes. When the layout rules evolve, the literal keeps
compiling and silently points at a directory that no longer exists —
`compile_cwd_from_output` and `BuildLayout` consumers then disagree
about where the build lives. See
[`agents/docs/path-conventions.md`](../../agents/docs/path-conventions.md).

## Use instead

| Instead of | Use |
|---|---|
| `".fbuild"` | `fbuild_paths::FBUILD_DIR_NAME` |
| `"build"` (the `.fbuild` child) | `fbuild_paths::BUILD_DIR_NAME` |
| `dir.join(".fbuild")` | `fbuild_paths::get_project_fbuild_dir(dir)` |
| `dir.join(".fbuild").join("build")` | `fbuild_paths::get_project_build_root(dir)` |
| `dir.join(".fbuild/build/<env>/<profile>")` | `fbuild_paths::BuildLayout::new(dir, env, profile).resolve()` |

Test fixtures that hand-roll a build tree should build it with
`BuildLayout::new(project, env, profile).resolve()` so the fixture
tracks the same collapse and override rules production does.

## Rollout

The lint is ON and denies new raw `.fbuild` literals by default.
[`src/allowlist.txt`](src/allowlist.txt) carries a **baseline** of the
legacy sites that existed when the lint landed
(FastLED/fbuild#1349). That list may only shrink:

1. Sanitize the file — route it through `fbuild_paths`.
2. Delete its line from `allowlist.txt`.
3. Bump the `version` in `Cargo.toml` so the Dylint `.so` cache is
   invalidated (`setup-soldr`'s cache key hashes the manifest, not
   `src/allowlist.txt`).

Adding a line is not allowed. If a call site needs a `.fbuild` path it
needs `fbuild_paths`, not an allowlist entry.

## Known limitations

The check is purely lexical on string-literal contents: any literal
containing `.fbuild` fires, including diagnostic messages that merely
mention the directory. Those are legitimate and belong on the allowlist
with a written justification. Segments assembled from non-literal
pieces are not detected.

## Running

```bash
soldr cargo dylint --all --workspace
```
