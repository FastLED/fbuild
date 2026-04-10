# CODEX.md

Codex working notes for this repo. Start with [CLAUDE.md](./CLAUDE.md) for the full project guide.

## Mandatory command rules

- Always run Rust tooling through `uv run` or the repo trampolines.
- Never run bare `cargo`, `rustc`, `rustfmt`, `clippy-driver`, `python`, or `pip`.
- Approved Rust forms in this repo are:
  - `uv run cargo ...`
  - `uv run rustc ...`
  - `uv run rustfmt ...`
  - `./_cargo ...`
  - `./_rustc ...`
  - `./_rustfmt ...`

## Why

- Repo hooks enforce this.
- `uv run cargo ...` works because `ci/dev-tools` registers `cargo`/`rustc`/`rustfmt` as repo-local uv scripts that dispatch through `ci/trampoline.py`.
- The uv scripts and shell trampolines both make sure the rustup-managed toolchain is used instead of stale system or Chocolatey installs.
- If you bypass them, you can hit wrong-toolchain errors or failures like `Cannot find .cargo/bin. Run ./install first.`

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

uv run python ci/zccache_setup.py
```

## Allowed fallback

- Repo trampolines: `_cargo`, `_rustc`, `_rustfmt`
- These are first-class approved paths, not second-class workarounds.
- Use `./_cargo`/`./_rustc`/`./_rustfmt` directly from the repo root when you want the shell trampoline form.

## If a command fails

- First check whether you used one of the approved wrapper forms above.
- If not, rerun it the required way before debugging anything else.
- If the pinned toolchain is missing, run `./install` via `uv run --script install`.
Read CLAUDE.md for repo instructions.