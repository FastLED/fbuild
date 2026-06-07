//! Back-reference graph synthesis + Graphviz `.dot` rendering.
//!
//! Consumes the `referenced_by` field that [`super::cref`] populates and
//! walks outward from a target symbol to surface "who pulled this in?".
//! Output is a Graphviz `.dot` string suitable for `dot -Tsvg`, inline
//! embedding in a Markdown report, or AI consumers parsing back-edges
//! directly from the textual form.
//!
//! The walker is **pure** — no I/O, no subprocess — so unit tests
//! exercise the full traversal and serialization path. The CLI and
//! markdown integrations in `fbuild-build` / `fbuild-cli` are thin
//! wrappers around this module.
//!
//! ## Termination strategy
//!
//! `ld --cref` gives us TU-granularity back-references. A naive
//! breadth-first walk explodes for hub symbols (`printf` has ~25
//! mbedTLS referencers; expanding two more hops produces an
//! unreadable wall). The default policy mixes three predicates so the
//! resulting graph stays readable without per-symbol tuning:
//!
//! 1. **Cross-archive termination.** Stop expanding a branch the
//!    first time it crosses out of the root symbol's archive. This
//!    surfaces the boundary where the symbol "escapes" its own
//!    library — exactly what bloat analysts want. `Fixed(N)` skips
//!    this rule.
//! 2. **Fan-out cap `K`.** Per node, sort referencing TUs by their
//!    *own* attributed flash bytes (sum of symbol sizes defined in
//!    that TU), keep the top `K`, and collapse the rest into a
//!    `[… and M more]` super-node.
//! 3. **Hard depth cap.** If neither of the above has fired by depth
//!    `max_depth`, bail. Default 4 — enough to escape one library +
//!    two intermediate hops + one app TU, but not enough to enumerate
//!    an IDF subsystem's entire fan-in.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use super::{FineGrainedSymbolMap, SymbolReference};

/// Traversal-depth policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GraphDepth {
    /// Stop expanding the first time a branch leaves the root
    /// symbol's archive. Still respects [`GraphConfig::max_depth`]
    /// as a safety belt.
    #[default]
    Adaptive,
    /// Walk exactly `N` hops outward, regardless of archive
    /// transitions. Useful when the caller wants a uniform-depth
    /// view (e.g. `--depth 2` for "everyone two hops out").
    Fixed(u32),
}

/// User-visible knobs for back-reference graph synthesis.
#[derive(Debug, Clone)]
pub struct GraphConfig {
    pub depth: GraphDepth,
    /// Per-node fan-out cap. Excess referencers collapse into a
    /// single `[… and N more]` super-node so dense hubs don't blow
    /// the graph up. Default 5.
    pub fan_out: usize,
    /// Hard cap on traversal depth even when `Adaptive` hasn't
    /// terminated. Default 4.
    pub max_depth: u32,
    /// Archives whose hops collapse into a single super-node per
    /// archive. Useful for the libc internal-wrapper case
    /// (`_vfprintf_r` → `libc_a-vprintf.o` → `libc_a-printf.o` →
    /// …); collapsing `libc.a` skips straight to the non-libc edges.
    pub collapse_archives: Vec<String>,
    /// Archives whose branches are dropped entirely. Use when the
    /// caller only cares about non-system referencers.
    pub exclude_archives: Vec<String>,
}

impl Default for GraphConfig {
    fn default() -> Self {
        Self {
            depth: GraphDepth::Adaptive,
            fan_out: 5,
            max_depth: 4,
            collapse_archives: vec!["libc.a".to_string(), "libgcc.a".to_string()],
            exclude_archives: Vec::new(),
        }
    }
}

/// One node in the back-reference graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphNode {
    /// Stable id used as the `.dot` node name.
    pub id: String,
    /// Display label rendered inside the node box.
    pub label: String,
    /// Archive group for coloring (`libc.a`, `libFastLED.a`, …).
    pub archive: Option<String>,
    /// Object file name (for non-collapsed nodes).
    pub object: Option<String>,
    pub kind: NodeKind,
    /// BFS distance from the root symbol (0 = root).
    pub depth: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeKind {
    /// The target symbol we're tracing. Always exactly one of these.
    RootSymbol { demangled: String, size: u64 },
    /// A translation unit that (transitively) references the root.
    /// `size_hint` is the sum of symbol sizes attributed to this TU
    /// in the report, used both for fan-out ranking and node sizing.
    TranslationUnit { size_hint: Option<u64> },
    /// A super-node bundling multiple TUs from the same archive
    /// because of `collapse_archives` OR fan-out overflow.
    Collapsed { archive: String, count: usize },
}

/// Directed edge from a referencer toward what it references.
/// (`from` referenced `to`.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphEdge {
    pub from: String,
    pub to: String,
}

/// Back-reference graph rooted at a single target symbol.
#[derive(Debug, Clone)]
pub struct BackrefGraph {
    pub root_id: String,
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

/// Index over a symbol map for fast "what symbols live in this TU?"
/// lookups during graph synthesis. Built once per analysis run.
pub struct TuIndex<'a> {
    /// `(archive, object) -> Vec<&FineGrainedSymbol>`. `archive: None`
    /// keeps bare-object TUs separate (`main.cpp.o` has no archive).
    by_tu: BTreeMap<(Option<String>, String), Vec<&'a super::FineGrainedSymbol>>,
}

impl<'a> TuIndex<'a> {
    pub fn build(map: &'a FineGrainedSymbolMap) -> Self {
        let mut by_tu: BTreeMap<(Option<String>, String), Vec<&'a super::FineGrainedSymbol>> =
            BTreeMap::new();
        for s in &map.symbols {
            let Some(obj) = s.object.as_ref() else {
                continue;
            };
            by_tu
                .entry((s.archive.clone(), obj.clone()))
                .or_default()
                .push(s);
        }
        Self { by_tu }
    }

    /// All symbols defined in a TU.
    pub fn symbols_in(&self, tu: &SymbolReference) -> &[&'a super::FineGrainedSymbol] {
        self.by_tu
            .get(&(tu.archive.clone(), tu.object.clone()))
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Total flash + ram bytes attributed to a TU. Used both for
    /// fan-out ranking (bigger contributors rank higher) and for
    /// node sizing in the rendered graph.
    pub fn bytes_in(&self, tu: &SymbolReference) -> u64 {
        self.symbols_in(tu).iter().map(|s| s.size).sum()
    }

    /// Aggregate every TU's contributed bytes for callers that want
    /// a global ranking (e.g. sidecar-file selection).
    pub fn tu_total_bytes(&self) -> BTreeMap<(Option<String>, String), u64> {
        let mut out: BTreeMap<(Option<String>, String), u64> = BTreeMap::new();
        for (key, syms) in &self.by_tu {
            let total: u64 = syms.iter().map(|s| s.size).sum();
            out.insert(key.clone(), total);
        }
        out
    }
}

impl BackrefGraph {
    /// Build the graph rooted at a target symbol (looked up by
    /// **mangled** name — matches `FineGrainedSymbol::mangled`).
    /// Returns a single-node graph when the symbol is unknown or
    /// has no referencers (the report should still embed something
    /// rather than crash; "no referencers" is a valid finding).
    pub fn build(map: &FineGrainedSymbolMap, target_mangled: &str, config: &GraphConfig) -> Self {
        let index = TuIndex::build(map);
        Self::build_with_index(map, &index, target_mangled, config)
    }

    /// Same as [`build`] but reuses a pre-built index — useful when
    /// emitting graphs for every top-N symbol (the per-symbol index
    /// rebuild would be O(N²)).
    pub fn build_with_index(
        map: &FineGrainedSymbolMap,
        index: &TuIndex<'_>,
        target_mangled: &str,
        config: &GraphConfig,
    ) -> Self {
        // Resolve the root symbol from the map.
        let root = map
            .symbols
            .iter()
            .find(|s| s.mangled == target_mangled || s.demangled == target_mangled);
        let Some(root) = root else {
            // Unknown symbol: emit a single isolated node so the
            // caller still gets a renderable .dot, not a parse error.
            let root_id = sanitize_id(target_mangled);
            return Self {
                root_id: root_id.clone(),
                nodes: vec![GraphNode {
                    id: root_id,
                    label: format!("{target_mangled}\n(not in symbol map)"),
                    archive: None,
                    object: None,
                    kind: NodeKind::RootSymbol {
                        demangled: target_mangled.to_string(),
                        size: 0,
                    },
                    depth: 0,
                }],
                edges: Vec::new(),
            };
        };

        let root_archive = root.archive.clone();
        let root_id = format!("sym__{}", sanitize_id(&root.mangled));
        let mut nodes: Vec<GraphNode> = vec![GraphNode {
            id: root_id.clone(),
            label: format_root_label(&root.demangled, root.size),
            archive: root.archive.clone(),
            object: root.object.clone(),
            kind: NodeKind::RootSymbol {
                demangled: root.demangled.clone(),
                size: root.size,
            },
            depth: 0,
        }];
        let mut edges: Vec<GraphEdge> = Vec::new();
        let mut node_id_by_tu: BTreeMap<(Option<String>, String), String> = BTreeMap::new();
        let mut visited_tus: BTreeSet<(Option<String>, String)> = BTreeSet::new();

        // BFS queue. Each entry: (current TU, target node id it points to, depth).
        let mut queue: VecDeque<((Option<String>, String), String, u32)> = VecDeque::new();

        // Seed level 1: every TU that directly references the root.
        let level1 = rank_and_cap_referencers(
            &root.referenced_by,
            index,
            config,
            &root_archive,
            /*depth=*/ 1,
        );
        for entry in level1 {
            match entry {
                CappedReferencer::Tu(tu_ref) => {
                    let key = (tu_ref.archive.clone(), tu_ref.object.clone());
                    if visited_tus.insert(key.clone()) {
                        let node_id = format!("tu__{}", sanitize_id(&tu_id_str(&tu_ref)));
                        let label = format_tu_label(&tu_ref, index.bytes_in(&tu_ref));
                        let size_hint = Some(index.bytes_in(&tu_ref));
                        nodes.push(GraphNode {
                            id: node_id.clone(),
                            label,
                            archive: tu_ref.archive.clone(),
                            object: Some(tu_ref.object.clone()),
                            kind: NodeKind::TranslationUnit { size_hint },
                            depth: 1,
                        });
                        node_id_by_tu.insert(key.clone(), node_id.clone());
                        edges.push(GraphEdge {
                            from: node_id.clone(),
                            to: root_id.clone(),
                        });
                        queue.push_back((key, node_id, 1));
                    }
                }
                CappedReferencer::CollapsedArchive { archive, count } => {
                    let node_id = format!("col__{}__d1", sanitize_id(&archive));
                    if !nodes.iter().any(|n| n.id == node_id) {
                        nodes.push(GraphNode {
                            id: node_id.clone(),
                            label: format!("[{archive}]\n{count} TUs"),
                            archive: Some(archive.clone()),
                            object: None,
                            kind: NodeKind::Collapsed { archive, count },
                            depth: 1,
                        });
                    }
                    edges.push(GraphEdge {
                        from: node_id,
                        to: root_id.clone(),
                    });
                }
                CappedReferencer::FanOutOverflow { count } => {
                    let node_id = format!("ovf__d1__{}", count);
                    nodes.push(GraphNode {
                        id: node_id.clone(),
                        label: format!("(… and {count} more)"),
                        archive: None,
                        object: None,
                        kind: NodeKind::Collapsed {
                            archive: "(overflow)".to_string(),
                            count,
                        },
                        depth: 1,
                    });
                    edges.push(GraphEdge {
                        from: node_id,
                        to: root_id.clone(),
                    });
                }
            }
        }

        // Expand: each TU's parents are the union of `referenced_by`
        // across every symbol defined in that TU.
        while let Some((current_key, current_id, depth)) = queue.pop_front() {
            if depth >= config.max_depth {
                continue;
            }
            // Adaptive termination: if we've already left the root
            // archive (and the root had an archive at all), stop
            // expanding past this node. Fixed-depth ignores this.
            let current_archive = current_key.0.clone();
            if matches!(config.depth, GraphDepth::Adaptive)
                && depth >= 1
                && root_archive.is_some()
                && current_archive != root_archive
            {
                continue;
            }
            let current_tu = SymbolReference {
                archive: current_key.0.clone(),
                object: current_key.1.clone(),
            };
            // Collect parent TUs ranked by their own attributed bytes.
            let mut parent_refs: Vec<SymbolReference> = Vec::new();
            let mut seen: BTreeSet<(Option<String>, String)> = BTreeSet::new();
            for sym in index.symbols_in(&current_tu) {
                for r in &sym.referenced_by {
                    let key = (r.archive.clone(), r.object.clone());
                    if key == current_key {
                        continue; // self-reference, skip
                    }
                    if seen.insert(key) {
                        parent_refs.push(r.clone());
                    }
                }
            }
            let next_depth = depth + 1;
            let capped =
                rank_and_cap_referencers(&parent_refs, index, config, &root_archive, next_depth);
            for entry in capped {
                match entry {
                    CappedReferencer::Tu(tu_ref) => {
                        let key = (tu_ref.archive.clone(), tu_ref.object.clone());
                        if !visited_tus.insert(key.clone()) {
                            // Already in the graph — just add the edge.
                            if let Some(target_id) = node_id_by_tu.get(&key) {
                                push_edge_dedup(
                                    &mut edges,
                                    GraphEdge {
                                        from: target_id.clone(),
                                        to: current_id.clone(),
                                    },
                                );
                            }
                            continue;
                        }
                        let node_id = format!("tu__{}", sanitize_id(&tu_id_str(&tu_ref)));
                        let label = format_tu_label(&tu_ref, index.bytes_in(&tu_ref));
                        let size_hint = Some(index.bytes_in(&tu_ref));
                        nodes.push(GraphNode {
                            id: node_id.clone(),
                            label,
                            archive: tu_ref.archive.clone(),
                            object: Some(tu_ref.object.clone()),
                            kind: NodeKind::TranslationUnit { size_hint },
                            depth: next_depth,
                        });
                        node_id_by_tu.insert(key.clone(), node_id.clone());
                        edges.push(GraphEdge {
                            from: node_id.clone(),
                            to: current_id.clone(),
                        });
                        queue.push_back((key, node_id, next_depth));
                    }
                    CappedReferencer::CollapsedArchive { archive, count } => {
                        let node_id = format!("col__{}__d{}", sanitize_id(&archive), next_depth);
                        if !nodes.iter().any(|n| n.id == node_id) {
                            nodes.push(GraphNode {
                                id: node_id.clone(),
                                label: format!("[{archive}]\n{count} TUs"),
                                archive: Some(archive.clone()),
                                object: None,
                                kind: NodeKind::Collapsed { archive, count },
                                depth: next_depth,
                            });
                        }
                        push_edge_dedup(
                            &mut edges,
                            GraphEdge {
                                from: node_id,
                                to: current_id.clone(),
                            },
                        );
                    }
                    CappedReferencer::FanOutOverflow { count } => {
                        let node_id = format!(
                            "ovf__d{}__{}__{}",
                            next_depth,
                            sanitize_id(&current_id),
                            count
                        );
                        nodes.push(GraphNode {
                            id: node_id.clone(),
                            label: format!("(… and {count} more)"),
                            archive: None,
                            object: None,
                            kind: NodeKind::Collapsed {
                                archive: "(overflow)".to_string(),
                                count,
                            },
                            depth: next_depth,
                        });
                        edges.push(GraphEdge {
                            from: node_id,
                            to: current_id.clone(),
                        });
                    }
                }
            }
        }

        Self {
            root_id,
            nodes,
            edges,
        }
    }

    /// Render the graph as a Graphviz `digraph` string.
    pub fn to_dot(&self) -> String {
        let mut out = String::new();
        out.push_str("digraph backref {\n");
        out.push_str("  rankdir=LR;\n");
        out.push_str("  node [shape=box, style=\"filled,rounded\", fontname=\"Helvetica\"];\n");
        out.push_str("  edge [fontname=\"Helvetica\", fontsize=10];\n");
        for n in &self.nodes {
            let color = node_color(n);
            let label = escape_dot_label(&n.label);
            let width = node_width(n);
            out.push_str(&format!(
                "  \"{}\" [label=\"{}\", fillcolor=\"{}\"{}];\n",
                n.id,
                label,
                color,
                if let Some(w) = width {
                    format!(", width={w:.2}")
                } else {
                    String::new()
                },
            ));
        }
        for e in &self.edges {
            out.push_str(&format!("  \"{}\" -> \"{}\";\n", e.from, e.to));
        }
        out.push_str("}\n");
        out
    }
}

/// Result of ranking and capping a single layer of referencers.
enum CappedReferencer {
    Tu(SymbolReference),
    CollapsedArchive { archive: String, count: usize },
    FanOutOverflow { count: usize },
}

/// Apply `exclude_archives`, `collapse_archives`, and the fan-out cap.
/// Returns a vector of survivors / collapsed buckets / overflow.
fn rank_and_cap_referencers(
    refs: &[SymbolReference],
    index: &TuIndex<'_>,
    config: &GraphConfig,
    _root_archive: &Option<String>,
    _depth: u32,
) -> Vec<CappedReferencer> {
    if refs.is_empty() {
        return Vec::new();
    }
    // 1. Drop excluded archives entirely.
    let mut kept: Vec<SymbolReference> = refs
        .iter()
        .filter(|r| {
            !r.archive
                .as_ref()
                .map(|a| config.exclude_archives.iter().any(|x| x == a))
                .unwrap_or(false)
        })
        .cloned()
        .collect();
    if kept.is_empty() {
        return Vec::new();
    }
    // 2. Collapse archives into super-nodes (one super-node per
    //    archive at this layer; the remaining TUs in non-collapsed
    //    archives fall through to fan-out ranking).
    let mut by_collapse: BTreeMap<String, Vec<SymbolReference>> = BTreeMap::new();
    let mut non_collapsed: Vec<SymbolReference> = Vec::new();
    for r in kept.drain(..) {
        let collapse_match = r
            .archive
            .as_ref()
            .map(|a| config.collapse_archives.iter().any(|x| x == a));
        if let Some(true) = collapse_match {
            let key = r.archive.clone().unwrap_or_default();
            by_collapse.entry(key).or_default().push(r);
        } else {
            non_collapsed.push(r);
        }
    }
    // 3. Rank non-collapsed by attributed bytes desc; apply K cap.
    non_collapsed.sort_by_key(|b| std::cmp::Reverse(index.bytes_in(b)));
    let mut out: Vec<CappedReferencer> = Vec::new();
    let total = non_collapsed.len();
    let take_n = config.fan_out.min(total);
    for tu in non_collapsed.into_iter().take(take_n) {
        out.push(CappedReferencer::Tu(tu));
    }
    let overflow = total.saturating_sub(take_n);
    if overflow > 0 {
        out.push(CappedReferencer::FanOutOverflow { count: overflow });
    }
    // 4. Emit collapsed super-nodes.
    for (archive, tus) in by_collapse {
        out.push(CappedReferencer::CollapsedArchive {
            archive,
            count: tus.len(),
        });
    }
    out
}

fn push_edge_dedup(edges: &mut Vec<GraphEdge>, candidate: GraphEdge) {
    if !edges
        .iter()
        .any(|e| e.from == candidate.from && e.to == candidate.to)
    {
        edges.push(candidate);
    }
}

/// Map an archive name to a Graphviz fill color. Coloring follows
/// the ecosystem the archive lives in so the eye jumps to the
/// non-system edges.
fn node_color(n: &GraphNode) -> &'static str {
    match (&n.kind, n.archive.as_deref()) {
        (NodeKind::RootSymbol { .. }, _) => "#ff6b6b", // red — the target
        (NodeKind::Collapsed { .. }, _) => "#e0e0e0",  // gray — super-node
        (_, Some("libc.a" | "libgcc.a" | "libm.a")) => "#cccccc",
        (_, Some(a)) if a.starts_with("libstdc++") => "#cccccc",
        (_, Some("libFastLED.a")) => "#ffd166",
        (_, Some(a)) if a.starts_with("libArduino") => "#06d6a0",
        (_, Some(a)) if a.starts_with("libesp") || a.starts_with("libfreertos") => "#118ab2",
        (_, Some(a)) if a.starts_with("libmbed") => "#a463f2",
        (_, Some(a)) if a.starts_with("liblog") => "#83c5be",
        (_, Some(a)) if a.starts_with("libheap") => "#f4a261",
        (_, None) => "#fdfdfd", // app-side (bare object), near-white
        _ => "#cfe2ff",         // other libs, soft blue
    }
}

/// Compute a Graphviz `width=` value sized by `log10(bytes + 1)`. Keeps
/// the visual size comparable across builds (huge symbols stay big
/// but don't dominate the page).
fn node_width(n: &GraphNode) -> Option<f64> {
    let bytes = match &n.kind {
        NodeKind::RootSymbol { size, .. } => *size,
        NodeKind::TranslationUnit {
            size_hint: Some(b), ..
        } => *b,
        _ => return None,
    };
    if bytes == 0 {
        return Some(1.0);
    }
    let base = (bytes as f64).log10().max(1.0);
    Some((base * 0.7).clamp(1.0, 4.5))
}

fn format_root_label(demangled: &str, size: u64) -> String {
    let truncated = if demangled.len() > 64 {
        format!("{}…", &demangled[..63])
    } else {
        demangled.to_string()
    };
    format!("{truncated}\n{size} B")
}

fn format_tu_label(tu: &SymbolReference, bytes: u64) -> String {
    match &tu.archive {
        Some(a) => format!("{}\n{a}\n{bytes} B", tu.object),
        None => format!("{}\n(app)\n{bytes} B", tu.object),
    }
}

fn tu_id_str(tu: &SymbolReference) -> String {
    match &tu.archive {
        Some(a) => format!("{a}__{}", tu.object),
        None => tu.object.clone(),
    }
}

/// Map any string into a Graphviz-safe id (alphanumerics + `_`).
pub fn sanitize_id(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        out.push_str("anon");
    }
    out
}

/// Map a demangled symbol name into a filesystem-safe filename
/// component (sidecar `.dot` file naming). Truncates at 80 chars so
/// long C++ template names don't blow the filesystem name limit.
pub fn sanitize_filename(symbol: &str) -> String {
    let mut out = String::with_capacity(symbol.len().min(80));
    for c in symbol.chars().take(80) {
        let safe = match c {
            '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*' | ' ' | '\t' | '\n' | '('
            | ')' | ',' | '&' | '#' | ';' => '_',
            other => other,
        };
        out.push(safe);
    }
    if out.is_empty() {
        out.push_str("sym");
    }
    out
}

fn escape_dot_label(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symbol_analysis::{FineGrainedSymbol, FineGrainedSymbolMap, SectionBytes};
    use crate::MemoryRegion;

    fn sym(
        mangled: &str,
        demangled: &str,
        size: u64,
        archive: Option<&str>,
        object: &str,
        refs: Vec<SymbolReference>,
    ) -> FineGrainedSymbol {
        FineGrainedSymbol {
            mangled: mangled.to_string(),
            demangled: demangled.to_string(),
            address: 0x1000,
            size,
            sym_type: 'T',
            region: MemoryRegion::Flash,
            archive: archive.map(|s| s.to_string()),
            object: Some(object.to_string()),
            output_section: Some(".flash.text".to_string()),
            source: "nm".to_string(),
            referenced_by: refs,
        }
    }

    fn refr(archive: Option<&str>, object: &str) -> SymbolReference {
        SymbolReference {
            archive: archive.map(|s| s.to_string()),
            object: object.to_string(),
        }
    }

    fn map(symbols: Vec<FineGrainedSymbol>) -> FineGrainedSymbolMap {
        FineGrainedSymbolMap {
            elf_path: "test.elf".to_string(),
            map_path: None,
            total_flash: symbols.iter().map(|s| s.size).sum(),
            total_ram: 0,
            symbols,
            sections: Vec::<SectionBytes>::new(),
        }
    }

    #[test]
    fn unknown_symbol_yields_single_node_graph() {
        let m = map(vec![sym(
            "main",
            "main",
            10,
            None,
            "main.cpp.o",
            Vec::new(),
        )]);
        let g = BackrefGraph::build(&m, "does_not_exist", &GraphConfig::default());
        assert_eq!(g.nodes.len(), 1);
        assert_eq!(g.edges.len(), 0);
        assert!(g.nodes[0].label.contains("not in symbol map"));
        // Renders without panicking.
        let dot = g.to_dot();
        assert!(dot.contains("digraph"));
    }

    #[test]
    fn symbol_with_no_referencers_yields_root_only() {
        let m = map(vec![sym(
            "__StackTop",
            "__StackTop",
            0,
            None,
            "linker.ld",
            Vec::new(),
        )]);
        let g = BackrefGraph::build(&m, "__StackTop", &GraphConfig::default());
        assert_eq!(g.nodes.len(), 1);
        assert!(g.edges.is_empty());
        assert!(matches!(g.nodes[0].kind, NodeKind::RootSymbol { .. }));
    }

    /// Issue #463 motivating case: `_vfprintf_r` defined in libc,
    /// referenced by libc-internal wrappers. Default config collapses
    /// libc.a so we see straight to the non-libc consumers.
    #[test]
    fn vfprintf_r_collapses_libc_chain() {
        let symbols = vec![
            sym(
                "_vfprintf_r",
                "_vfprintf_r",
                11_309,
                Some("libc.a"),
                "libc_a-vfprintf.o",
                vec![
                    refr(Some("libc.a"), "libc_a-vprintf.o"),
                    refr(Some("libc.a"), "libc_a-printf.o"),
                    refr(Some("libc.a"), "libc_a-fprintf.o"),
                ],
            ),
            sym(
                "vprintf",
                "vprintf",
                500,
                Some("libc.a"),
                "libc_a-vprintf.o",
                vec![refr(Some("liblog.a"), "log_write.c.obj")],
            ),
            sym(
                "printf",
                "printf",
                500,
                Some("libc.a"),
                "libc_a-printf.o",
                vec![refr(Some("libmbedcrypto.a"), "sha512.c.obj")],
            ),
            sym(
                "fprintf",
                "fprintf",
                500,
                Some("libc.a"),
                "libc_a-fprintf.o",
                vec![refr(Some("libsdmmc.a"), "sdmmc_io.c.obj")],
            ),
        ];
        let g = BackrefGraph::build(&map(symbols), "_vfprintf_r", &GraphConfig::default());
        // Root + one collapsed-libc super-node ⇒ no individual libc
        // referencer should appear at depth 1.
        let libc_internal_tus: Vec<_> = g
            .nodes
            .iter()
            .filter(|n| {
                matches!(n.kind, NodeKind::TranslationUnit { .. })
                    && n.archive.as_deref() == Some("libc.a")
            })
            .collect();
        assert!(
            libc_internal_tus.is_empty(),
            "libc.a should be collapsed; got {libc_internal_tus:?}"
        );
        let collapsed: Vec<_> = g
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, NodeKind::Collapsed { .. }))
            .collect();
        assert!(!collapsed.is_empty(), "expected collapsed super-node");
    }

    /// FastLED ctor case: app-side TU references the ctor directly.
    /// Adaptive termination ends at depth 1 because main.cpp.o is
    /// outside libFastLED.a.
    #[test]
    fn fastled_ctor_adaptive_terminates_at_app() {
        let symbols = vec![
            sym(
                "_ZN8FastLED5beginEv",
                "FastLED::begin()",
                400,
                Some("libFastLED.a"),
                "fl.cpp.o",
                vec![refr(None, "main.cpp.o")],
            ),
            sym(
                "setup",
                "setup",
                30,
                None,
                "main.cpp.o",
                vec![refr(None, "main.cpp.o")], // self-ref noise
            ),
        ];
        let g = BackrefGraph::build(
            &map(symbols),
            "_ZN8FastLED5beginEv",
            &GraphConfig::default(),
        );
        // root + main.cpp.o
        assert_eq!(g.nodes.len(), 2);
        assert!(g
            .nodes
            .iter()
            .any(|n| n.object.as_deref() == Some("main.cpp.o")));
    }

    #[test]
    fn fan_out_cap_overflow_emits_super_node() {
        // 10 referencers, cap = 3 ⇒ 3 TUs + 1 overflow super-node.
        let mut refs = Vec::new();
        for i in 0..10 {
            refs.push(refr(Some("libapp.a"), &format!("caller_{i}.o")));
        }
        let mut symbols = vec![sym(
            "hub",
            "hub",
            100,
            Some("libcore.a"),
            "core.o",
            refs.clone(),
        )];
        // Give each caller a distinct attributed-bytes ranking so the
        // top-K selection is deterministic.
        for (i, _) in refs.iter().enumerate() {
            symbols.push(sym(
                &format!("caller_{i}_sym"),
                &format!("caller_{i}_sym"),
                (10 - i) as u64,
                Some("libapp.a"),
                &format!("caller_{i}.o"),
                Vec::new(),
            ));
        }
        let cfg = GraphConfig {
            fan_out: 3,
            collapse_archives: Vec::new(),
            ..GraphConfig::default()
        };
        let g = BackrefGraph::build(&map(symbols), "hub", &cfg);
        let tu_nodes = g
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, NodeKind::TranslationUnit { .. }))
            .count();
        assert_eq!(tu_nodes, 3, "expected exactly 3 TUs after fan-out cap");
        let overflow = g.nodes.iter().find(
            |n| matches!(&n.kind, NodeKind::Collapsed { archive, .. } if archive == "(overflow)"),
        );
        assert!(overflow.is_some(), "expected an overflow super-node");
        assert!(overflow.unwrap().label.contains("7 more"));
    }

    #[test]
    fn exclude_archive_drops_branches() {
        let symbols = vec![
            sym(
                "target",
                "target",
                100,
                None,
                "main.o",
                vec![
                    refr(Some("libdrop.a"), "dropped.o"),
                    refr(Some("libkeep.a"), "kept.o"),
                ],
            ),
            sym(
                "dropped_sym",
                "dropped_sym",
                5,
                Some("libdrop.a"),
                "dropped.o",
                Vec::new(),
            ),
            sym(
                "kept_sym",
                "kept_sym",
                5,
                Some("libkeep.a"),
                "kept.o",
                Vec::new(),
            ),
        ];
        let cfg = GraphConfig {
            exclude_archives: vec!["libdrop.a".to_string()],
            collapse_archives: Vec::new(),
            ..GraphConfig::default()
        };
        let g = BackrefGraph::build(&map(symbols), "target", &cfg);
        let archives: BTreeSet<Option<String>> = g
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, NodeKind::TranslationUnit { .. }))
            .map(|n| n.archive.clone())
            .collect();
        assert!(archives.contains(&Some("libkeep.a".to_string())));
        assert!(!archives.contains(&Some("libdrop.a".to_string())));
    }

    #[test]
    fn dot_serialization_includes_all_nodes_and_edges() {
        let symbols = vec![
            sym(
                "a",
                "a",
                10,
                Some("libA.a"),
                "a.o",
                vec![refr(Some("libB.a"), "b.o")],
            ),
            sym("b", "b", 20, Some("libB.a"), "b.o", Vec::new()),
        ];
        let cfg = GraphConfig {
            collapse_archives: Vec::new(),
            ..GraphConfig::default()
        };
        let g = BackrefGraph::build(&map(symbols), "a", &cfg);
        let dot = g.to_dot();
        assert!(dot.starts_with("digraph"));
        assert!(dot.contains("rankdir=LR"));
        for n in &g.nodes {
            assert!(dot.contains(&n.id), "node {} missing from .dot", n.id);
        }
        for e in &g.edges {
            assert!(
                dot.contains(&format!("\"{}\" -> \"{}\"", e.from, e.to)),
                "edge {}->{} missing",
                e.from,
                e.to
            );
        }
    }

    #[test]
    fn sanitize_id_replaces_unsafe_chars() {
        assert_eq!(sanitize_id("foo bar"), "foo_bar");
        assert_eq!(sanitize_id("a:b/c\\d"), "a_b_c_d");
        assert_eq!(sanitize_id("_ok_123"), "_ok_123");
        assert_eq!(sanitize_id(""), "anon");
    }

    #[test]
    fn sanitize_filename_truncates_and_strips() {
        let long = "a".repeat(200);
        assert_eq!(sanitize_filename(&long).len(), 80);
        assert_eq!(sanitize_filename("op<<(int)"), "op___int_");
        assert_eq!(sanitize_filename(""), "sym");
    }
}
