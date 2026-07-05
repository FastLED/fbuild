# Path conventions: prefix roots, factory functions, and cross-project keys

**Reach for this doc before you construct, join, compare, or hash any
filesystem path** — especially cache directories, build directories, and
anything that feeds a cache key. Getting the *prefix root* wrong is silent:
the code compiles, the build "works", and the only symptom is a cache that
never hits (or, worse, a cache that hits when it shouldn't). Two real bugs
in FastLED/fbuild#952 came from exactly this.

## The prefix root is part of the path — never invent one

Every fbuild path is rooted at a **prefix** chosen by a factory function.
There are two families, and they are not interchangeable:

| Prefix root | Factory | Example contents |
|---|---|---|
| Global cache — `~/.fbuild/{dev\|prod}/cache/` | `fbuild_paths::get_cache_root()` → `Cache::new(project_dir).*_dir()` | `packages/`, `toolchains/`, `platforms/`, `libraries/`, `core/` |
| Global fbuild root — `~/.fbuild/{dev\|prod}/` | `fbuild_paths::get_fbuild_root()` | `daemon/`, `cache/`, `tmp/`, `zccache/` |
| Per-project build — `<project>/.fbuild/build/{env}/{profile}/` | `fbuild_paths::BuildLayout` / `get_project_build_dir(project_dir)` | `core/`, `src/`, `fw_libs/`, `firmware.elf`, `firmware.bin` |

Load-bearing rules:

- **The `{dev\|prod}` segment is chosen by `is_dev_mode()`** inside the
  factory. Never hardcode `prod` (or `dev`) into a path — call the factory
  so dev-mode isolation (`FBUILD_DEV_MODE=1` → port 8865, `~/.fbuild/dev/`)
  keeps working.
- **Global caches are keyed by content/signature, not by project.**
  `Cache::new(project_dir).core_artifacts_dir()` returns
  `~/.fbuild/{dev|prod}/cache/core` and *ignores* `project_dir` — the `project_dir`
  argument is only there so the same `Cache` handle can also resolve
  per-project dirs. If you want a cache shared across projects, it must live
  under `get_cache_root()`, and its **key must not encode the project
  directory** (see next section).
- **Never fabricate a placeholder root like `/project` or `/tmp/x`** in
  code — even in tests. Use `tempfile::TempDir` for a real, normalized,
  auto-cleaned path. A literal `/…` root reads as production intent and
  hides spelling/prefix bugs.

## Double-check that the paths you compare actually align

The #942/#952 caching bug: two builds of the *same* project from different
directories (`/tmp/nds` vs `/tmp/nds2`) produced different cache keys, so the
global cache never hit. Root cause: project-specific `-I<dir>` include flags
leaked into the signature, and a raw `Path::starts_with(project_dir)` failed
to strip them because the include-dir spelling didn't match the
canonicalized `project_dir` spelling.

When a value must be compared against, or excluded relative to, a root:

- **Normalize both sides through the same factory before comparing.** Use
  `fbuild_core::path::normalize_for_key` (strips `\\?\`, folds separators to
  `/`, case-folds on Windows/macOS) or compare `NormalizedPath` values. Do
  **not** hand-roll `starts_with` on raw `Path`s that may be spelled
  differently (canonicalized vs not, `\\?\` prefix, trailing slash, case) —
  that is exactly the comparison that silently missed and defeated the cache.
- **A cross-project cache key must be project-independent.** Before hashing,
  strip (or relativize) anything rooted under `project_dir` — include dirs,
  `-I`/`-isystem`/`-iquote` flags, and build-dir paths. A key that encodes
  the project directory gives every project directory a distinct key.
- **A zccache-visible compile key is workspace-relative or it won't hit
  across project dirs.** Compiles routed through
  `crate::compiler::compile_source` relativize source/`-o`/`-I` to the
  workspace root via `zccache::compile_cwd_from_output` +
  `path_arg_for_compile_cwd` + `normalize_flags_for_compile_cwd`, and run
  with `cwd = workspace`. An invocation that passes **absolute** paths with
  `cwd = None` (as the legacy `library_compiler` fw-libs path did) bakes the
  project directory into the key and misses the cache on every fresh project.

## Checklist before you commit a path change

1. Which prefix root is this? Did you get it from a factory, or hardcode it?
2. Is `{dev|prod}` resolved by `is_dev_mode()`, not a literal?
3. If it's a cache key: does it accidentally encode the project directory,
   the build directory, or an absolute machine-specific path?
4. If you compare paths: are both sides normalized through
   `normalize_for_key` / a `NormalizedPath`?
5. In tests: real `TempDir`, never a `/placeholder` root.

See `crates/fbuild-paths/src/lib.rs` (roots + `BuildLayout`),
`crates/fbuild-packages/src/cache.rs` (cache subdir layout),
`crates/fbuild-core/src/path.rs` (`NormalizedPath`, `normalize_for_key`),
and `crates/fbuild-build/src/zccache.rs` (workspace relativization).
