# `ini_parser` module

PlatformIO INI parser, split into focused submodules to stay below the per-file
LOC gate.

## Submodules

- **`mod.rs`** -- Public API: `PlatformIOConfig` struct, constructors, and
  getters (`get_env_config`, `get_build_flags`, `get_lib_deps`, etc.).
- **`parser.rs`** -- Raw INI tokenizer (`parse_ini`), `[env:*]` inheritance
  resolution (`resolve_all_envs`, `resolve_env`, `resolve_section`), and
  `${section.key}` variable substitution driver (`substitute_vars`).
- **`variables.rs`** -- Variable-reference lookup helpers
  (`resolve_variable`, `resolve_section_key`) that follow `extends` chains.
- **`values.rs`** -- Value-string helpers: `strip_inline_comment`,
  `parse_flags`, `parse_lib_deps`, `parse_list_values`.
- **`tests.rs`** -- Unit tests for the public API and the parsing helpers.
