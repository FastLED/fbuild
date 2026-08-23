# CI and Development Tools

Python scripts for CI, packaging, and development tooling. All invoked via `uv run`.

## Contents

- **`build_dist.py`** -- Triggers GitHub Actions native builds, downloads artifacts, and assembles `dist/` for PyPI packaging
- **`build_qemu_linux_runtime.py`** -- Builds the Linux runtime-library bundle Espressif QEMU needs (`ldd` closure minus glibc, built on ubuntu:22.04). Published to the `qemu-linux-runtime-v1` release and downloaded on demand by `fbuild-toolchain`'s `esp_qemu_runtime` module; run in CI by `qemu-runtime-bundle.yml`
- **`check_workspace_crates.py`** -- Monocrate guard: fails if the root `Cargo.toml` `[workspace] members` list gains a crate outside the approved allowlist (run by `crate-gate.yml`)
- **`check_workflow_concurrency.py`** -- Requires every `pull_request`-triggered workflow to declare an auto-cancel `concurrency:` block, so pushing again to a feature branch supersedes its in-flight runs instead of queueing ~80 more board builds. Run by `ci-workflow-drift.yml`; exemptions (reusable templates, `hw-ci.yml`, `add-to-project.yml`) carry a reason in the script. Tested by `test_workflow_concurrency.py`. See [.github/workflows/README.md](../.github/workflows/README.md#concurrency-auto-cancel-superseded-pr-runs).
- **`check_rust_toolchain_pins.py`** -- Prevents fbuild-owned Rust 1.95.0 MSRV, toolchain, workflow, and bootstrap declarations from drifting
- **`enforce_platform_boundary.py`** -- Independent whole-tree and manifest checker for the exact host-platform occurrence ledger
- **`env.py`** -- Centralized PATH activation ensuring the Rust tool bin directory is on PATH before invoking Rust tools
- **`extract_pio_build_flags.py`** -- Extracts compiler/linker flags from PlatformIO for each board and writes reference JSONs
- **`lint.py`** -- Workspace linting (rustfmt + clippy), supports single-file and auto-fix modes
- **`platform_boundary_research.py`** -- Host-independent phase-1 inventory and cross-host drift check for FastLED/fbuild#1307
- **`render_workflows.py`** -- Re-renders the `on:` and `concurrency:` blocks of `.github/workflows/build-*.yml` and the full `nightly-platforms.yml` from `board_families.json` + `ci_common_paths.txt`. CI invokes `--check` to enforce no drift. See [docs/DEVELOPMENT.md](../docs/DEVELOPMENT.md#ci-per-board-build-triggers) and FastLED/fbuild#835.
- **`board_families.json`** -- SOT: per-board metadata (workflow / test_dir / env_name / family) plus the family → crate-path mapping consumed by `render_workflows.py`.
- **`ci_common_paths.txt`** -- SOT: paths whose changes force-run *every* per-board build workflow.
- **`test.py`** -- Workspace test runner with `--full` (stress + integration) and per-crate filtering
- **`trampoline.py`** -- Development helpers that run fbuild workspace binaries through soldr-managed Cargo
- **`validate_boards.py`** -- Validates fbuild board JSON assets against PlatformIO board definitions
- **`zccache_setup.py`** -- Optional local wrapper-mode setup for zccache; not used by the standard soldr build path

## Subdirectories

- **`dev-tools/`** -- Pip-installable package that provides soldr and repo-local development helper scripts
- **`hooks/`** -- Claude Code hook scripts (tool guard, lint, readme guard, session lifecycle)
