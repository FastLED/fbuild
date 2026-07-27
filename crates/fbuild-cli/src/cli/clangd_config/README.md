# clangd_config

`fbuild clangd-config`: emits an IDE-ready clangd configuration for a
project's PlatformIO environment. Split into an editor-neutral core plus
per-editor emitters (FastLED/fbuild#1076 Phase 0) so the core is reusable by
the planned `fbuild ide` command.

## Modules

- **`mod.rs`** -- `run_clangd_config` entry point, `Editor` selection
  (`--editor vscode|zed`), `.clangd` emission (shared/editor-neutral),
  compile-DB freshness (`ensure_compile_db`, gated by `--refresh`), and
  `emit_editor_config` dispatch. `ensure_compile_db`, `emit_clangd_file`, and
  `emit_editor_config` are `pub(crate)` so the future `fbuild ide` module can
  call them directly.
- **`vscode.rs`** -- VS Code emitter: merge-don't-clobber
  `.vscode/settings.json` (clangd args including
  `--compile-commands-dir=${workspaceFolder}`) and write-once
  `.vscode/extensions.json`.
- **`zed.rs`** -- Zed emitter: merge-don't-clobber `.zed/settings.json`
  (`file_types."C++"` += `"ino"`, `lsp.clangd.binary.arguments` without
  `--compile-commands-dir` — Zed has no `${workspaceFolder}`-style variable,
  so it relies on `.clangd`'s `CompilationDatabase: .` instead).

## Why `.clangd` has no `Compiler:` pin

Earlier versions pinned `Compiler:` in `.clangd` and asked clangd to
`--query-driver` the real cross-compiler for its builtin include search
paths. That path had silently degenerated to a no-op glob:
`CompileDatabase::translate_for_clang` (in `fbuild-build-engine`) rewrites
`arguments[0]` to bare `clang`/`clang++` before the database is written, so
`extract_compiler_path` could only ever recover `"clang++"`. Phase 0 deleted
that machinery and instead bakes the toolchain's GCC builtin include dirs
into every translated entry as `-isystem` args — see
`crates/fbuild-build-engine/src/compile_database/clang.rs`.
