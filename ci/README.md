# CI and Development Tools

Python scripts for CI, packaging, and development tooling. All invoked via `uv run`.

`local_gate.py` runs both required Ubuntu jobs through bosn's pinned act2
engine, then replays the entire existing `dylint.yml` workflow with the ordinary
unlabelled PR event. It checks clean source identity, native Linux x64 execution,
all three Dylint job verdicts and the completed policy, library and workspace
steps. The wrapper uses released bosn 0.1.13 for the stock runner tools.
`local-gate.toml` enforces PR attestations. The local gate uses the existing
workflow-dispatch event for the Ubuntu pair and PR/minimal for Dylint before
stamping the tree. Dispatch Dylint adds cross-target work, so it is not used as
a substitute for the ordinary PR selection. Library UI fixtures retain their
existing source-change condition. Shared replay coverage binds every Ubuntu action and command to its literal
execution name, including the verifier prerequisite. Its event decision executes
on dispatch; the attestation check remains required on PRs. The original Ubuntu
terminal report is forwarded unchanged to the pinned shared checker; Dylint
retains its additional private command checks and still runs before stamping. Board builds, extended/full
mode and other native hosts retain their remote coverage. The migration is
tracked in [#1635](https://github.com/FastLED/fbuild/issues/1635), with Dylint
parity and complete cold/warm measurements in [#1643](https://github.com/FastLED/fbuild/issues/1643).

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
- **`select_boards.py`** -- Picks which `build-<board>.yml` workflows `nightly-platforms.yml` dispatches: path-selected from a push diff, or `--all` for the nightly badge refresh. Tested by `test_select_boards.py`.
- **`test_setup_soldr_cache_keys.py`** -- Every saving `zackees/setup-soldr` step must set a `cache-key-suffix` unique per runner; jobs sharing a key race to own one immutable cache entry.
- **`board_families.json`** -- SOT: per-board metadata (workflow / test_dir / env_name / family) plus the family → crate-path mapping consumed by `render_workflows.py`.
- **`ci_common_paths.txt`** -- SOT: paths whose changes force-run *every* per-board build workflow.
- **`test.py`** -- Workspace test runner with `--full` (stress + integration) and per-crate filtering
- **`trampoline.py`** -- Development helpers that run fbuild workspace binaries through soldr-managed Cargo
- **`validate_boards.py`** -- Validates fbuild board JSON assets against PlatformIO board definitions
- **`zccache_setup.py`** -- Optional local wrapper-mode setup for zccache; not used by the standard soldr build path

## Subdirectories

- **`dev-tools/`** -- Pip-installable package that provides soldr and repo-local development helper scripts
- **`hooks/`** -- Claude Code hook scripts (tool guard, lint, readme guard, session lifecycle)

- **`test_cache_status.py`** streams the unchanged Cargo test selection with JSON compiler evidence; only a completed assertion failure enables setup-soldr failure saves. Compiler errors, incomplete logs and cancellation remain blocked.

- **`check_python_complexity.py`** -- Runs configured C901/RUF100 checks on every tracked Python file, including scripts excluded from normal discovery (FastLED/fbuild#1639).
