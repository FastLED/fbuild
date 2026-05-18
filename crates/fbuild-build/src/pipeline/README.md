# pipeline

Shared build pipeline helpers used by all platform orchestrators.

This module was split out of a single `pipeline.rs` file to satisfy the
per-file LOC gate. The public API surface is unchanged — every item
previously exported from `crate::pipeline` is re-exported here.

Submodules:

- `build_unflags` — `apply_build_unflags`, `apply_debug_build_type`,
  `remove_unflagged_tokens`, and related flag-cleanup helpers.
- `project_discovery` — include-path discovery and platform/library detection.
- `compile` — source compilation helpers, local library compilation,
  `compile_commands.json` generation, toolchain version logging.
- `link` — link result logging and final `BuildResult` assembly.
- `library` — `LibraryBuildEnv`, `pick_archiver`, and project-as-library
  compilation.
- `sequential` — the sequential compile -> link -> result pipeline used by
  most non-ESP32 platforms.
