# `uv run` / `uv sync` Optimization Report

## Goal

User's stated goal: *"rebuild incrementally when rust changes, don't tolerate
stale artifacts."* uv should auto-trigger rebuild when a `.rs` file is edited,
the rebuild should be at the cargo-incremental floor, and untouched-source
reinstalls (version bumps, lockfile churn) should be effectively free.

## Two distinct problems were getting conflated

1. **Forced reinstall path** — `uv sync --reinstall-package fbuild` rebuilds
   the wheel via setup.py → `soldr cargo build`. Even when no source had
   changed, this was 14.9s (cold cargo cache in a temp dir from PEP 517
   build isolation).
2. **Real-edit rebuild path** — when an `.rs` file actually changes,
   `cargo build --release` cascaded through 8+ first-party crates with
   opt-level=3 codegen + slow `link.exe` linker → **100s** for a single
   one-line edit.

## Fixes applied (two PRs, both merged)

### PR #743 — no-source-change reinstall path

- **`CARGO_TARGET_DIR` pinned** to `~/.fbuild/cargo-target/wheel-build`
  (absolute, persistent). Survives PEP 517 temp-dir copies. Deliberately
  separate from `<repo>/target/` so it doesn't churn against `soldr cargo
  build` from the dev CLI.
- **mtime-skip in `BuildWithCargo.run`**: if the staged binary is newer than
  every `.rs` / `Cargo.toml` / `Cargo.lock` / `rust-toolchain.toml`, skip the
  cargo invocation entirely.
- **`[tool.uv] no-build-isolation-package = ["fbuild"]`** — build runs in
  the real repo against the real venv, not a temp copy. mtime-skip can then
  see the persistent `ci/bin/` staged binary.
- **`default-groups = ["dev"]`** + setuptools added to the `dev` group —
  setuptools is present in the venv when uv (re)builds the wheel, no
  chicken-and-egg.
- **`[tool.uv] cache-keys = [..., "crates/**/*.rs", ...]`** — uv re-syncs
  the editable fbuild install when any of these change. **This is what
  prevents the "stale artifact" failure.** Without it, `.rs` edits silently
  produce a `_native.pyd` mismatch.

### PR #744 — real-edit rebuild path

- **`setup.py` defaults to the dev profile**, not `--release`. Set
  `FBUILD_BUILD_RELEASE=1` to opt back in for perf tests. The PyPI release
  flow bypasses setup.py (it calls `cargo zigbuild --release` directly in
  `release-auto.yml`), so published wheels are unaffected.
- **`[profile.dev.package."*"] opt-level = 3`** in `Cargo.toml` — third-party
  deps stay optimized even in dev profile, so runtime hot paths (serde,
  tokio, reqwest) don't tank.
- **`rust-lld` as the Windows linker** via `.cargo/config.toml`. Ships with
  the Rust toolchain, faster than `link.exe`, cross-profile.

## Measurements

All scenarios use a *real* content edit (append `\n` to the file), not just a
mtime touch — touched-but-unchanged files would hit zccache on rustc and not
exercise the real rebuild path.

### Headline: real `.rs` edit + `uv run python --version`

| Build profile (setup.py) | Time |
|---|---:|
| Release (pre-#744) | **100.1s** |
| Dev (post-#744, current default) | **18.9s** |

**5.3× speedup** on the path that fires when a Rust source actually changes.

### No source change + forced reinstall

| | Pre-#743 | Post-#743 |
|---|---:|---:|
| `uv sync --reinstall-package fbuild` | **14.9s** | **1.1s** |

**13.6× speedup**. mtime-skip never invokes cargo.

### Warm `uv run` (no edit at all)

| | Pre | Post |
|---|---:|---:|
| `uv run python --version` | ~110ms | ~100ms |

Unchanged — uv's audit-only path is already optimal.

## What's left: the touch-only / soldr overhead floor

Touch-only edits (any tool bumps a `.rs` file's mtime without changing
content) still cost ~14s in the `uv run` path. Profiling pinned it precisely:

| Component | Time |
|---|---:|
| Cargo's own `Finished` report | 1.8s |
| **soldr + zccache wrapper overhead (RUSTC_WRAPPER='' baseline)** | **7.6s** |
| uv sync + reinstall (mtime-skip fires, zero cargo) | 1.5s |
| **Total touch-only rebuild via `uv run`** | **~9-15s** |

The 5 consecutive identical builds spanned 7.6s — 19s. The variance
correlates with `--private-daemon` lifecycle behavior in soldr's zccache
wrapper. Filed upstream as **[soldr#883](https://github.com/zackees/soldr/issues/883)**
with full reproduction script and environment data. When that's resolved,
the touch-only path should land closer to ~3-4s.

## How to reproduce

```bash
python ci/bench_uv_run.py <label>
```

Writes `ci/bench-results/<label>.json`. Existing snapshots:

- `baseline.json` — `main` before either PR. Forced-reinstall = 14.9s.
- `after_fixes.json` — after PR #743 (mtime-skip path). Forced-reinstall = 1.1s.

The real-edit-cost measurements in this report were taken with timed manual
runs (`time uv run python --version` after `echo "" >> crates/fbuild-core/src/lib.rs`)
because `bench_uv_run.py` doesn't yet model a semantic edit — it only does
mtime touches, which fail to exercise the dev-vs-release codegen difference.

## Files

- `setup.py` — `CARGO_TARGET_DIR` pin, `_staged_binary_is_up_to_date`,
  `_use_release_profile()` env-gated profile selection.
- `pyproject.toml` — `no-build-isolation-package`, `default-groups = ["dev"]`,
  `cache-keys`; setuptools added to `dev` group.
- `Cargo.toml` — `[profile.dev.package."*"] opt-level = 3`.
- `.cargo/config.toml` — `[target.x86_64-pc-windows-msvc] linker = "rust-lld.exe"`.
- `ci/bench_uv_run.py` — benchmark script.
- `ci/bench-results/{baseline,after_fixes}.json` — raw timings.
