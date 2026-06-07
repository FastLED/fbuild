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

mod walker;
use walker::{push_edge_dedup, rank_and_cap_referencers, walk_forward, CappedReferencer};
pub use walker::{rank_callees_dual, CalleeRanked};

#[cfg(test)]
mod tests;

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

/// Which graph direction(s) to synthesize.
///
/// `Backward` is the historical default and what every caller pre-#463
/// asked for: walk from the root symbol *outward* along
/// `referenced_by` to surface "who pulled this in?".
///
/// `Forward` walks the inverse: along `references_to` (populated from
/// `objdump -d`) to surface "what does this symbol call?". Forward
/// edges are per-symbol, not per-TU — the AI-assisted-optimization
/// use-case (see #471 motivation) wants exactly that precision so it
/// can tell the difference between `ClocklessIdf5` calling `fl::sort`
/// (which it doesn't) vs. its TU calling it (which it might).
///
/// `Bidirectional` walks both. The rendered `.dot` shows backward
/// callers on one side of the root and forward callees on the other,
/// so a single picture answers both "who pulled this in?" and "what
/// did this end up dragging along?".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Direction {
    /// Backward only. Compatible with the pre-#471 default.
    #[default]
    Backward,
    /// Forward only. Useful when you want a small `.dot` focused on
    /// callees without the noise of the call-in side.
    Forward,
    /// Both directions from the root.
    Bidirectional,
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
    /// Which direction(s) to walk from the root. See [`Direction`].
    /// Default `Backward` matches the pre-#471 behaviour exactly.
    pub direction: Direction,
}

impl Default for GraphConfig {
    fn default() -> Self {
        Self {
            depth: GraphDepth::Adaptive,
            fan_out: 5,
            max_depth: 4,
            collapse_archives: vec!["libc.a".to_string(), "libgcc.a".to_string()],
            exclude_archives: Vec::new(),
            direction: Direction::Backward,
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
    /// A callee — a symbol the root (or one of its callees) calls.
    /// Distinct from `TranslationUnit` because forward edges are
    /// per-symbol, not per-TU. `size` is the callee's own flash
    /// footprint (drives ranking + node sizing).
    Callee {
        demangled: String,
        size: u64,
        callers_count: usize,
    },
    /// A super-node bundling multiple TUs from the same archive
    /// because of `collapse_archives` OR fan-out overflow.
    Collapsed { archive: String, count: usize },
}

/// Direction of a single edge in the graph.
///
/// `Backward`: caller → callee, drawn from a referencer toward the
/// root (matches the pre-#471 contract that `from referenced to`).
///
/// `Forward`: root → callee, drawn from the symbol toward the things
/// it calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EdgeDirection {
    #[default]
    Backward,
    Forward,
}

/// Directed edge. For `Backward` edges `from` referenced `to`; for
/// `Forward` edges `from` calls `to`. The renderer styles the two
/// flavours differently so a bidirectional graph stays readable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphEdge {
    pub from: String,
    pub to: String,
    pub direction: EdgeDirection,
}

impl GraphEdge {
    /// A back-edge from a referencer toward what it references.
    pub fn backward(from: String, to: String) -> Self {
        Self {
            from,
            to,
            direction: EdgeDirection::Backward,
        }
    }

    /// A forward edge from a caller toward what it calls.
    pub fn forward(from: String, to: String) -> Self {
        Self {
            from,
            to,
            direction: EdgeDirection::Forward,
        }
    }
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

    /// Same as [`Self::build`] but reuses a pre-built index — useful when
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

        // ---- Backward direction: seed level 1 with the TUs that
        // ---- directly reference the root.
        //
        // Skipped entirely when the caller asked for `Direction::Forward`
        // only — the BFS expansion below short-circuits naturally on an
        // empty queue, so we get a clean forward-only graph without
        // editing the rest of this method.
        let want_backward = matches!(
            config.direction,
            Direction::Backward | Direction::Bidirectional
        );
        let level1: Vec<CappedReferencer> = if want_backward {
            rank_and_cap_referencers(
                &root.referenced_by,
                index,
                config,
                &root_archive,
                /*depth=*/ 1,
            )
        } else {
            Vec::new()
        };
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
                        edges.push(GraphEdge::backward(node_id.clone(), root_id.clone()));
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
                    edges.push(GraphEdge::backward(node_id, root_id.clone()));
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
                    edges.push(GraphEdge::backward(node_id, root_id.clone()));
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
                                    GraphEdge::backward(target_id.clone(), current_id.clone()),
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
                        edges.push(GraphEdge::backward(node_id.clone(), current_id.clone()));
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
                            GraphEdge::backward(node_id, current_id.clone()),
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
                        edges.push(GraphEdge::backward(node_id, current_id.clone()));
                    }
                }
            }
        }

        // ---- Forward direction: walk root.references_to outward.
        //
        // Forward edges are per-symbol (not TU-level), so the node
        // identity here is `sym__<mangled>` instead of `tu__<archive>__<obj>`.
        // This is the side of the graph that answers "what does
        // ClocklessIdf5 actually call?" — the motivating ask in #471
        // when an AI optimization pass mistook `fl::sort` (a sibling
        // in the same TU) for something the root symbol called.
        if matches!(
            config.direction,
            Direction::Forward | Direction::Bidirectional
        ) {
            let mut visited_syms: BTreeSet<String> = BTreeSet::new();
            visited_syms.insert(root.mangled.clone());
            walk_forward(
                map,
                config,
                root,
                &root_id,
                /*depth=*/ 1,
                &mut nodes,
                &mut edges,
                &mut visited_syms,
            );
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
            // Forward edges (caller → callee) get a distinct dashed
            // arrow + a `forward` label so a bidirectional graph
            // doesn't blur the two directions together. Backward
            // edges keep the historical solid arrow (no extra
            // attributes) so existing `.dot` consumers don't see a
            // diff from the visual they're used to.
            match e.direction {
                EdgeDirection::Backward => {
                    out.push_str(&format!("  \"{}\" -> \"{}\";\n", e.from, e.to));
                }
                EdgeDirection::Forward => {
                    out.push_str(&format!(
                        "  \"{}\" -> \"{}\" [style=dashed, color=\"#0066cc\", fontcolor=\"#0066cc\", label=\"calls\"];\n",
                        e.from, e.to
                    ));
                }
            }
        }
        out.push_str("}\n");
        out
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
        NodeKind::Callee { size, .. } => *size,
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
