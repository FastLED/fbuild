# `graph/` — back-reference graph synthesis + `.dot` rendering

Split into submodules so every file stays under the 1000-LOC CI gate
while preserving the public API previously exposed as
`fbuild_core::symbol_analysis::graph::*`:

- **`mod.rs`** — public types (`GraphConfig`, `GraphDepth`, `Direction`,
  `GraphNode`, `NodeKind`, `EdgeDirection`, `GraphEdge`, `BackrefGraph`,
  `TuIndex`), the `BackrefGraph::build*` BFS driver, `to_dot()`
  rendering, and the dot-formatting helpers (`sanitize_id`,
  `sanitize_filename`, label/color/width helpers).
- **`walker.rs`** — walker internals (`CappedReferencer`,
  `rank_and_cap_referencers`, `push_edge_dedup`, `walk_forward`) and
  callee ranking (`CalleeCandidate`, `format_callee_label`,
  `rank_callees_dual`, `CalleeRanked`). `rank_callees_dual` and
  `CalleeRanked` are re-exported from `mod.rs` so external consumers
  keep the original import path.
- **`tests.rs`** — `#[cfg(test)]` suite covering the full walker +
  serialization path; only loaded by `mod.rs` under `#[cfg(test)]`.
