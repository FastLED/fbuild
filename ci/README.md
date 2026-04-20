# CI and Development Tools

Python scripts for CI, packaging, and development tooling. All invoked via `uv run`.

## Contents

- **`build_dist.py`** -- Triggers GitHub Actions native builds, downloads artifacts, and assembles `dist/` for PyPI packaging
- **`env.py`** -- Centralized PATH activation ensuring `.cargo/bin` is on PATH before invoking Rust tools
- **`extract_pio_build_flags.py`** -- Extracts compiler/linker flags from PlatformIO for each board and writes reference JSONs
- **`lint.py`** -- Workspace linting (rustfmt + clippy), supports single-file and auto-fix modes
- **`test.py`** -- Workspace test runner with `--full` (stress + integration) and per-crate filtering
- **`trampoline.py`** -- Rust toolchain trampolines (cargo, rustc, rustfmt, clippy-driver) registered as `uv` project scripts
- **`validate_boards.py`** -- Validates fbuild board JSON assets against PlatformIO board definitions
- **`zccache_setup.py`** -- Optional local wrapper-mode setup for zccache; not used by the standard soldr build path

## Subdirectories

- **`dev-tools/`** -- Pip-installable package that registers Rust tool trampolines as console scripts
- **`hooks/`** -- Claude Code hook scripts (tool guard, lint, readme guard, session lifecycle)
