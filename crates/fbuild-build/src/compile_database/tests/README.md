# compile_database tests

Unit and adversarial tests for the `compile_database` module, split across
several files so no single file exceeds the project's 1000-LOC gate.

## Files

- **`mod.rs`** -- Declares the test submodules and exposes a shared
  `generate_entries` shim that wraps `LanguageExtraFlags` for tests that only
  care about the common-flag path.
- **`serialization_and_write.rs`** -- `CompileEntry` JSON serialization,
  `CompileDatabase` container behavior, `write` / `write_and_copy`,
  `expected_output_path`, and `is_library_project` detection.
- **`cache_wrapper.rs`** -- `strip_cache_wrapper` (sccache / zccache / ccache).
- **`generate.rs`** -- `generate_entries`: extension-based compiler routing,
  path handling, argument structure, adversarial edge cases.
- **`clang.rs`** -- `translate_flags_for_clang`, `should_remove_flag`,
  `CompileDatabase::translate_for_clang`, and `prepare_for_iwyu` (IWYU prep).
