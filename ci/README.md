# CI and Development Tools

Python scripts for CI, packaging, and development tooling. All invoked via `uv run`.

## Contents

- **`build_dist.py`** -- Triggers GitHub Actions native builds, downloads artifacts, and assembles `dist/` for PyPI packaging
- **`check_workspace_crates.py`** -- Monocrate guard: fails if the root `Cargo.toml` `[workspace] members` list gains a crate outside the approved allowlist (run by `crate-gate.yml`)
- **`env.py`** -- Centralized PATH activation ensuring `.cargo/bin` is on PATH before invoking Rust tools
- **`extract_pio_build_flags.py`** -- Extracts compiler/linker flags from PlatformIO for each board and writes reference JSONs
- **`lint.py`** -- Workspace linting (rustfmt + clippy), supports single-file and auto-fix modes
- **`render_workflows.py`** -- Re-renders the `on:` blocks of `.github/workflows/build-*.yml` and the full `nightly-platforms.yml` from `board_families.json` + `ci_common_paths.txt`. CI invokes `--check` to enforce no drift. See [docs/DEVELOPMENT.md](../docs/DEVELOPMENT.md#ci-per-board-build-triggers) and FastLED/fbuild#835.
- **`board_families.json`** -- SOT: per-board metadata (workflow / test_dir / env_name / family) plus the family → crate-path mapping consumed by `render_workflows.py`.
- **`ci_common_paths.txt`** -- SOT: paths whose changes force-run *every* per-board build workflow.
- **`test.py`** -- Workspace test runner with `--full` (stress + integration) and per-crate filtering
- **`trampoline.py`** -- Development helpers that run fbuild workspace binaries through soldr-managed Cargo
- **`validate_boards.py`** -- Validates fbuild board JSON assets against PlatformIO board definitions
- **`zccache_setup.py`** -- Optional local wrapper-mode setup for zccache; not used by the standard soldr build path

## Subdirectories

- **`dev-tools/`** -- Pip-installable package that provides soldr and repo-local development helper scripts
- **`hooks/`** -- Claude Code hook scripts (tool guard, lint, readme guard, session lifecycle)
