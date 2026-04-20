# CODEX.md

Codex working notes for this repo. Start with [CLAUDE.md](./CLAUDE.md) for the full project guide.

## Mandatory command rules

- Always run Rust tooling through `uv run`, `soldr`, or the repo trampolines.
- Never run bare `cargo`, `rustc`, `rustfmt`, `clippy-driver`, `python`, or `pip`.
- Approved Rust forms in this repo are:
  - `uv run cargo ...`
  - `uv run rustc ...`
  - `uv run rustfmt ...`
  - `soldr cargo ...`
  - `soldr rustc ...`
  - `soldr rustfmt ...`
  - `./_cargo ...`
  - `./_rustc ...`
  - `./_rustfmt ...`

## Why

- Repo hooks enforce this.
- All three forms dispatch through [soldr](https://github.com/zackees/soldr), which resolves each tool via `rustup which` so the rustup-managed toolchain is always used instead of a stale system or Chocolatey install.
- `uv run cargo ...` works because `ci/dev-tools` registers `cargo`/`rustc`/`rustfmt` as repo-local uv scripts that now dispatch through `ci/trampoline.py` → `soldr`.
- The normal Cargo path is `soldr cargo ...`; do not add repo-specific `RUSTC_WRAPPER` wiring for standard builds.
- If you bypass them, you can hit wrong-toolchain errors.

## Use these

```powershell
uv run cargo check --workspace --all-targets
uv run cargo test -p fbuild-build -- --ignored
uv run cargo clippy --workspace --all-targets -- -D warnings
uv run cargo fmt --all

./_cargo check --workspace --all-targets
./_cargo test -p fbuild-build -- --ignored
./_cargo clippy --workspace --all-targets -- -D warnings
./_rustfmt --check crates/fbuild-build/src/compiler.rs

uv run test
uv run test --full
uv run test -p fbuild-build -- some_test_name
```

## Optional wrapper-mode

```powershell
uv run python ci/zccache_setup.py
```

This configures `rustc-wrapper = "zccache"` for local wrapper-mode experiments. Standard builds should use `uv run`, `soldr`, or the `_cargo`/`_rustc`/`_rustfmt` trampolines above.

## Allowed fallback

- Repo trampolines: `_cargo`, `_rustc`, `_rustfmt`
- These are first-class approved paths, not second-class workarounds.
- Use `./_cargo`/`./_rustc`/`./_rustfmt` directly from the repo root when you want the shell trampoline form.

## If a command fails

- First check whether you used one of the approved wrapper forms above.
- If not, rerun it the required way before debugging anything else.
- If the pinned toolchain is missing, run `./install` via `uv run --script install`.
Read CLAUDE.md for repo instructions.
