# CODEX.md

Codex working notes for this repo. Start with [CLAUDE.md](./CLAUDE.md) for the full project guide.

## Mandatory command rules

- Always run Rust tooling through `soldr` or `uv run soldr`.
- Never run bare `cargo`, `rustc`, `rustfmt`, `clippy-driver`, `python`, or `pip`.
- Approved Rust forms in this repo are:
  - `soldr cargo ...`
  - `soldr rustc ...`
  - `soldr rustfmt ...`
  - `uv run soldr cargo ...`
  - `uv run soldr rustc ...`
  - `uv run soldr rustfmt ...`

## Why

- Repo hooks enforce this.
- [soldr](https://github.com/zackees/soldr) resolves each tool via `rustup which` so the rustup-managed toolchain is always used instead of a stale system or Chocolatey install.
- `uv run soldr ...` works because `ci/dev-tools` installs `soldr` into the repo-local uv environment.
- The normal Cargo path is `soldr cargo ...`, so repo Rust builds use soldr's managed zccache path by default; do not add repo-specific `RUSTC_WRAPPER` wiring for standard builds.
- If you bypass them, you can hit wrong-toolchain errors.

## Use these

```powershell
uv run soldr cargo check --workspace --all-targets
uv run soldr cargo test -p fbuild-build -- --ignored
uv run soldr cargo clippy --workspace --all-targets -- -D warnings
uv run soldr cargo fmt --all

soldr cargo check --workspace --all-targets
soldr cargo test -p fbuild-build -- --ignored
soldr cargo clippy --workspace --all-targets -- -D warnings
soldr rustfmt --check crates/fbuild-build/src/compiler.rs

uv run test
uv run test --full
uv run test -p fbuild-build -- some_test_name
```

## Optional wrapper-mode

```powershell
uv run python ci/zccache_setup.py
```

This configures `rustc-wrapper = "zccache"` for local wrapper-mode experiments. Standard builds should use `soldr` or `uv run soldr` above.

## Fallback

- Use `uv run soldr ...` when `soldr` is not on PATH but the repo-local uv environment is available.

## If a command fails

- First check whether you used one of the approved wrapper forms above.
- If not, rerun it the required way before debugging anything else.
- If the pinned toolchain is missing, run `./install` via `uv run --script install`.
Read CLAUDE.md for repo instructions.
