# `source_scanner/`

Sibling-directory submodules for `source_scanner.rs` (the project source
discovery + filtering + prototype-extraction code). Today this is just
`tests.rs` — the original `#[cfg(test)] mod tests { ... }` block was
lifted out of the parent file as a separate file so the parent stays
under the 1000-LOC gate (see `.github/workflows/loc-gate.yml`).

When growing this module, prefer cohesive per-domain submodules
(`filter.rs` for the include/exclude rule engine, `prototypes.rs` for
the tree-sitter-based forward-declaration extraction, …) over letting
the parent file balloon back over the limit. The split convention is
documented in the root `CLAUDE.md`.
