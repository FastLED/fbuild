# `ban_raw_fbuild_path`

This lint bans string literals that spell the `.fbuild` directory
segment by hand and directs developers at `fbuild_paths` — the crate
that declares itself the single source of truth for all `.fbuild`
paths.

## Why

`crates/fbuild-paths/src/lib.rs:3` says it owns every `.fbuild` path,
but the layout underneath it is not a fixed string:

- the `<env>` segment is **dropped** whenever `BuildLayout.flatten_env`
  is set or the project directory's basename already equals the env name
  — so the same build can live at `.../build/<env>/<profile>/` or at
  `.../build/<profile>/`,
- an explicit `override_root`, and then `FBUILD_BUILD_DIR`, each
  **replace the root wholesale** ahead of the `<project>/.fbuild/build`
  default,
- PlatformIO-style projects stage each board at `.build/pio/<board>/`
  and build with `env == board`, which is exactly the basename-matches
  case above — the tree is `.build/pio/<board>/.fbuild/build/<profile>/`,
  not `.../build/<board>/<profile>/`.

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

Adding a line is not allowed, and that is enforced rather than merely
requested: `ci/check_fbuild_path_baseline.py` diffs this file against
`origin/main` and fails the Dylint job on any added entry. Removals are
always fine. If a call site needs a `.fbuild` path it needs
`fbuild_paths`, not an allowlist entry.

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
