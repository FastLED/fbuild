# `uv run` / `uv sync` Optimization Report

## Goal

User's stated goal: *"rebuild incrementally when rust changes, don't tolerate
stale artifacts."* uv should auto-trigger rebuild when a `.rs` file is edited,
and that rebuild should be at the soldr-incremental floor.

## What was actually wrong

Two distinct problems were getting conflated:

1. **Forced reinstall path** — `uv sync --reinstall-package fbuild` rebuilds
   the wheel via setup.py → `soldr cargo build`. Even when no source had
   actually changed, this was 25-30s (cold cargo cache in a temp dir from
   PEP 517 build isolation).
2. **Edit-detection path** — when fbuild is editable (which it is:
   `source = { editable = "." }` in uv.lock), uv decides whether to re-sync
   based on `[tool.uv] cache-keys = [...]`. The default cache-keys only
   watches `pyproject.toml`. So `.rs` edits were silently producing **stale
   artifacts** — `uv run fbuild ...` would use whatever `_native.pyd` was
   last built, no matter what you edited.

## Fixes applied

### setup.py
- **`CARGO_TARGET_DIR` pinned** to `~/.fbuild/cargo-target/wheel-build`
  (absolute, persistent). Survives PEP 517 temp-dir copies. Deliberately
  separate from `<repo>/target/` so it doesn't churn against `soldr cargo
  build` from the dev CLI.
- **mtime-skip in `BuildWithCargo.run`**: if the staged binary is newer than
  every `.rs` / `Cargo.toml` / `Cargo.lock` / `rust-toolchain.toml`, skip the
  cargo invocation entirely.

### pyproject.toml
- **`[tool.uv] no-build-isolation-package = ["fbuild"]`** — build runs in
  the real repo against the real venv, not a temp copy. mtime-skip can then
  see the persistent `ci/bin/` staged binary.
- **`default-groups = ["dev"]`** + setuptools added to the `dev` group —
  setuptools is present in the venv when uv (re)builds the wheel, no
  chicken-and-egg.
- **`[tool.uv] cache-keys = [..., "crates/**/*.rs", ...]`** — uv re-syncs
  the editable fbuild install when any of these change. **This is what
  prevents the "stale artifact" failure.** It also means `uv run` after a
  `.rs` edit now actually costs cargo + linker time (no free lunch).

## Measurements

All scenarios use a real content edit (append `\n` to the file), not just a
mtime touch — touched-but-unchanged files would hit zccache on rustc and
not exercise the real rebuild path.

### Scenario 1 — forced reinstall, no source change

This fires on version bumps, lockfile churn, explicit `--reinstall-package
fbuild`. The mtime-skip fast path:

| | Baseline | After fixes | Speedup |
|---|---:|---:|---:|
| `uv sync --reinstall-package fbuild` | **14.9s** | **1.1s** | **13.6×** |

### Scenario 2 — real `.rs` edit + `uv run` (cache-keys watching `.rs`)

This is the "no stale artifacts" path:

| | Baseline | After fixes |
|---|---:|---:|
| `.rs` edit + `uv run python --version` (round 1) | 15.7s | 14.4s |
| `.rs` edit + `uv run python --version` (round 2, warmer cache) | 15.9s | 14.3s |

**Only ~1-2s saved on this path.** The bottleneck is cargo recompiling
`fbuild-core` (zccache misses because content actually changed) + cascading
to dependents + linking `fbuild-cli` on Windows. zccache doesn't cache the
link step. `CARGO_INCREMENTAL=1` made no measurable difference on this
workspace (release profile already strips intermediates).

### Scenario 3 — warm `uv run` (no edit)

| | Baseline | After fixes |
|---|---:|---:|
| `uv run python --version` | 110ms | 100ms |

Unchanged. uv's audit-only path is already optimal.

## What's left

The 14s edit-rebuild is at cargo's incremental + linker floor for this
workspace. Pushing further requires build-tool-level changes:

- **Faster linker** (rust-lld / mold). On Windows, `[target.x86_64-pc-windows-msvc]
  linker = "rust-lld.exe"` in `.cargo/config.toml` typically cuts link time
  by 30-50%. Risk: occasional linker compat issues. Not applied here — would
  affect the dev CLI's `target/` too and needs broader testing.
- **Split `fbuild-cli` into multiple smaller binaries**. Each link would be
  cheaper. Out of scope.
- **Migrate off `setup.py` + hand-rolled `ci/publish.py` to `setuptools-rust`**.
  Wouldn't affect the rebuild floor, but would make the build-system code
  much smaller and remove the dual-path-divergence class of bugs that
  prompted this whole investigation. Separate refactor (see earlier thread).

## Files changed

- `setup.py` — `CARGO_TARGET_DIR` pin + `_staged_binary_is_up_to_date`
  helper + mtime-skip wired into `BuildWithCargo.run`.
- `pyproject.toml` — `no-build-isolation-package`, `default-groups`,
  `cache-keys`; setuptools added to `dev`.
- `ci/bench_uv_run.py` — benchmark script (new).
- `ci/bench-results/{baseline,after_fixes}.json` — raw timings.
- `ci/bench-results/REPORT.md` — this file.
