# symbol_analyzer

Driver around `fbuild_core::symbol_analysis` that runs the cross
toolchain (`nm`, `c++filt`, `objdump`) against an ELF and produces a
fully-attributed `FineGrainedSymbolMap`, plus formatters that turn the
map into text or Markdown reports.

Split into submodules to keep individual files under the 1000-LOC CI
gate:

- `mod.rs` — toolchain invocation (`analyze_elf`,
  `run_objdump_and_attribute`), helpers (`read_pt_load_regions`,
  `derive_cppfilt_path`, `default_map_path`, `demangle_batch`,
  `AnalyzeConfig`), the text-report formatter, and ELF/project
  discovery (`discover_elf_in_project` and friends). Re-exports the
  markdown surface so existing call sites keep resolving.
- `markdown.rs` — `format_markdown_report*`, `MarkdownGraphOptions`,
  `SidecarOptions`, `write_sidecar_dot_files`, plus the internal
  graph-section helpers.
- `tests.rs` — the unit tests for everything in the module.
