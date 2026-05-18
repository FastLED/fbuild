# compile_database

Generates `compile_commands.json` (clangd-compatible JSON compilation database)
so IDE features like "Go to Definition" work with real include paths instead of
GCC response files (`@file`).

## Modules

- **`types.rs`** -- `CompileEntry`, `CompileDatabase` struct, `TargetArchitecture` enum.
- **`database.rs`** -- File IO (`write`, `write_and_copy`, `expected_output_path`) and `is_library_project` detection.
- **`cache_wrapper.rs`** -- `strip_cache_wrapper`: strips sccache/zccache/ccache wrappers from arg lists.
- **`clang.rs`** -- GCC-to-clang flag translation and IWYU (include-what-you-use) preparation.
- **`generate.rs`** -- `generate_entries`: builds entries from compiler flags and a list of sources.
- **`tests.rs`** -- Unit and adversarial tests for the whole module.

This module was split out of a single `compile_database.rs` file to satisfy the
1000-LOC-per-file gate enforced in CI.
