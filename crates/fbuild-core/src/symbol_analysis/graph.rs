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

/// BFS the per-symbol forward edges (`references_to`) outward from a
/// starting symbol. Adds one `Callee` node per surviving callee, one
/// `Forward` edge from caller → callee, and a single
/// `(… and N more)` overflow super-node when the fan-out exceeds
/// `config.fan_out`. Recurses to `config.max_depth`.
///
/// Ranking is by callee flash size (heaviest first), which matches
/// the AI-optimization use-case (#471): when a model sees a top
/// symbol it wants to know "what's the biggest thing this calls?",
/// not "what's the most-shared thing this calls?". The
/// most-shared view is surfaced via [`rank_callees_dual`] for the
/// markdown sub-table — both axes coexist; the graph picks one.
#[allow(clippy::too_many_arguments)]
fn walk_forward(
    map: &FineGrainedSymbolMap,
    config: &GraphConfig,
    caller_sym: &super::FineGrainedSymbol,
    caller_node_id: &str,
    depth: u32,
    nodes: &mut Vec<GraphNode>,
    edges: &mut Vec<GraphEdge>,
    visited_syms: &mut BTreeSet<String>,
) {
    if depth > config.max_depth {
        return;
    }
    // Resolve callee names to FineGrainedSymbols where possible. Names
    // that don't resolve (typically platform/libc symbols not pulled
    // into the report) are kept as raw mangled strings with size=0
    // so the graph still surfaces the edge.
    let mut callees: Vec<CalleeCandidate<'_>> = Vec::new();
    for callee_mangled in &caller_sym.references_to {
        if visited_syms.contains(callee_mangled) {
            continue;
        }
        let resolved = map
            .symbols
            .iter()
            .find(|s| s.mangled == *callee_mangled || s.demangled == *callee_mangled);
        callees.push(CalleeCandidate {
            mangled: callee_mangled.clone(),
            sym: resolved,
        });
    }
    if callees.is_empty() {
        return;
    }

    // Apply exclude_archives to filter callees from system libs the
    // caller doesn't want surfaced.
    callees.retain(|c| match c.sym.and_then(|s| s.archive.as_ref()) {
        Some(a) => !config.exclude_archives.iter().any(|x| x == a),
        None => true,
    });
    if callees.is_empty() {
        return;
    }

    // Rank by callee size (largest first); ties broken by
    // referenced_by length (most-shared first) so the ranking is
    // deterministic.
    callees.sort_by(|a, b| {
        b.size()
            .cmp(&a.size())
            .then_with(|| b.callers_count().cmp(&a.callers_count()))
            .then_with(|| a.mangled.cmp(&b.mangled))
    });

    let total = callees.len();
    let take_n = config.fan_out.min(total);
    let overflow = total.saturating_sub(take_n);

    for c in callees.iter().take(take_n) {
        let callee_id = format!("sym__{}", sanitize_id(&c.mangled));
        if visited_syms.insert(c.mangled.clone()) {
            let (demangled, size) = match c.sym {
                Some(s) => (s.demangled.clone(), s.size),
                None => (c.mangled.clone(), 0),
            };
            let callers_count = c.callers_count();
            let archive = c.sym.and_then(|s| s.archive.clone());
            let object = c.sym.and_then(|s| s.object.clone());
            nodes.push(GraphNode {
                id: callee_id.clone(),
                label: format_callee_label(&demangled, size, callers_count),
                archive,
                object,
                kind: NodeKind::Callee {
                    demangled,
                    size,
                    callers_count,
                },
                depth,
            });
            // Recurse — only if we have a resolved symbol with its
            // own references_to.
            if let Some(resolved) = c.sym {
                if depth < config.max_depth && !resolved.references_to.is_empty() {
                    walk_forward(
                        map,
                        config,
                        resolved,
                        &callee_id,
                        depth + 1,
                        nodes,
                        edges,
                        visited_syms,
                    );
                }
            }
        }
        push_edge_dedup(
            edges,
            GraphEdge::forward(caller_node_id.to_string(), callee_id),
        );
    }

    if overflow > 0 {
        let overflow_id = format!(
            "fwd_ovf__{}__d{}__{}",
            sanitize_id(caller_node_id),
            depth,
            overflow
        );
        nodes.push(GraphNode {
            id: overflow_id.clone(),
            label: format!("(… and {overflow} more callees)"),
            archive: None,
            object: None,
            kind: NodeKind::Collapsed {
                archive: "(overflow)".to_string(),
                count: overflow,
            },
            depth,
        });
        edges.push(GraphEdge::forward(caller_node_id.to_string(), overflow_id));
    }
}

/// Working struct for ranking callees: carries the mangled name plus
/// an optional resolved symbol pointer for size/popularity lookup.
struct CalleeCandidate<'a> {
    mangled: String,
    sym: Option<&'a super::FineGrainedSymbol>,
}

impl<'a> CalleeCandidate<'a> {
    fn size(&self) -> u64 {
        self.sym.map(|s| s.size).unwrap_or(0)
    }

    fn callers_count(&self) -> usize {
        self.sym.map(|s| s.referenced_by.len()).unwrap_or(0)
    }
}

/// Per-callee node label: `<demangled>\n<size> B\nshared with <N>`.
fn format_callee_label(demangled: &str, size: u64, callers_count: usize) -> String {
    let truncated = if demangled.len() > 64 {
        format!("{}…", &demangled[..63])
    } else {
        demangled.to_string()
    };
    if callers_count > 1 {
        format!("{truncated}\n{size} B\nshared ×{callers_count}")
    } else {
        format!("{truncated}\n{size} B")
    }
}

/// Rank a symbol's direct callees by two axes simultaneously and
/// return the top-`top_n` from each — the data the markdown
/// "Top N callees" sub-table renders side by side.
///
/// `(by_size, by_popularity, other_count)`:
///   - `by_size`: heaviest callees (flash bytes), ranked by callee
///     size descending.
///   - `by_popularity`: most-shared callees (how many TUs / symbols
///     also call them), ranked by `referenced_by.len()` descending.
///   - `other_count`: number of unique callees that fell out of both
///     top-N buckets (the "other" pile the user asked for).
///
/// Callees that don't resolve to a row in `map` (e.g. weak refs not
/// pulled in) get `size=0` and contribute to neither axis.
#[must_use]
pub fn rank_callees_dual<'a>(
    map: &'a FineGrainedSymbolMap,
    caller: &super::FineGrainedSymbol,
    top_n: usize,
) -> (Vec<CalleeRanked<'a>>, Vec<CalleeRanked<'a>>, usize) {
    // Resolve every callee once.
    let mut resolved: Vec<CalleeRanked<'a>> = caller
        .references_to
        .iter()
        .map(|m| {
            let s = map
                .symbols
                .iter()
                .find(|s| s.mangled == *m || s.demangled == *m);
            CalleeRanked {
                mangled: m.clone(),
                demangled: s.map(|s| s.demangled.clone()).unwrap_or_else(|| m.clone()),
                size: s.map(|s| s.size).unwrap_or(0),
                callers_count: s.map(|s| s.referenced_by.len()).unwrap_or(0),
                sym: s,
            }
        })
        .collect();

    // Sort copies by each axis. We clone the small structs because
    // they're cheap (4 fields + a `&FineGrainedSymbol`).
    let mut by_size = resolved.clone();
    by_size.sort_by(|a, b| {
        b.size
            .cmp(&a.size)
            .then_with(|| a.demangled.cmp(&b.demangled))
    });
    let mut by_popularity = resolved.clone();
    by_popularity.sort_by(|a, b| {
        b.callers_count
            .cmp(&a.callers_count)
            .then_with(|| b.size.cmp(&a.size))
            .then_with(|| a.demangled.cmp(&b.demangled))
    });

    let size_top = by_size.iter().take(top_n).cloned().collect::<Vec<_>>();
    let pop_top = by_popularity
        .iter()
        .take(top_n)
        .cloned()
        .collect::<Vec<_>>();

    // "Other" is the unique callees NOT present in either top-N
    // bucket.
    let top_mangled: BTreeSet<&str> = size_top
        .iter()
        .chain(pop_top.iter())
        .map(|c| c.mangled.as_str())
        .collect();
    let other_count = resolved
        .drain(..)
        .filter(|c| !top_mangled.contains(c.mangled.as_str()))
        .count();

    (size_top, pop_top, other_count)
}

/// Row in the dual-ranked callee table.
#[derive(Debug, Clone)]
pub struct CalleeRanked<'a> {
    pub mangled: String,
    pub demangled: String,
    pub size: u64,
    pub callers_count: usize,
    pub sym: Option<&'a super::FineGrainedSymbol>,
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
            references_to: Vec::new(),
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

    // ---- #471: forward + bidirectional traversal ----

    /// Helper: build a symbol with explicit `references_to`.
    fn sym_calls(
        mangled: &str,
        size: u64,
        archive: Option<&str>,
        object: &str,
        calls: Vec<&str>,
    ) -> FineGrainedSymbol {
        let mut s = sym(mangled, mangled, size, archive, object, Vec::new());
        s.references_to = calls.into_iter().map(|c| c.to_string()).collect();
        s
    }

    /// Forward-only build with no back-references: just root + its
    /// direct callees, ranked by callee size.
    #[test]
    fn forward_only_emits_callee_nodes() {
        let symbols = vec![
            sym_calls(
                "ClocklessIdf5",
                10_000,
                Some("libFastLED.a"),
                "clockless_idf5.cpp.o",
                vec!["esp_log_write", "rmt_tx_start", "fl_lerp8"],
            ),
            sym(
                "esp_log_write",
                "esp_log_write",
                2_000,
                Some("libesp.a"),
                "log.c.o",
                Vec::new(),
            ),
            sym(
                "rmt_tx_start",
                "rmt_tx_start",
                800,
                Some("libesp.a"),
                "rmt.c.o",
                Vec::new(),
            ),
            sym(
                "fl_lerp8",
                "fl_lerp8",
                60,
                Some("libFastLED.a"),
                "math.cpp.o",
                Vec::new(),
            ),
        ];
        let cfg = GraphConfig {
            direction: Direction::Forward,
            collapse_archives: Vec::new(),
            ..GraphConfig::default()
        };
        let g = BackrefGraph::build(&map(symbols), "ClocklessIdf5", &cfg);
        // Root + 3 callees.
        let callees: Vec<_> = g
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, NodeKind::Callee { .. }))
            .collect();
        assert_eq!(callees.len(), 3, "expected three forward callees");
        // All edges should be forward.
        assert!(g
            .edges
            .iter()
            .all(|e| e.direction == EdgeDirection::Forward));
        // Heaviest callee (esp_log_write @ 2000) ranks first by size.
        let heaviest = callees
            .iter()
            .max_by_key(|n| match &n.kind {
                NodeKind::Callee { size, .. } => *size,
                _ => 0,
            })
            .unwrap();
        assert!(heaviest.label.starts_with("esp_log_write"));
    }

    /// The motivating bug from #471: when a symbol's TU has siblings,
    /// the forward graph must show ONLY what the root symbol itself
    /// calls — not what the sibling symbols call. The cref-inversion
    /// approach would have surfaced `fl::sort` here (called by a
    /// sibling in the same TU); the objdump-based per-symbol
    /// `references_to` correctly omits it.
    #[test]
    fn forward_excludes_tu_siblings_callees() {
        let symbols = vec![
            // Root: calls only esp_log_write.
            sym_calls(
                "ClocklessIdf5",
                10_000,
                Some("libFastLED.a"),
                "clockless_idf5.cpp.o",
                vec!["esp_log_write"],
            ),
            // Sibling in the same TU: calls fl::sort. This must NOT
            // appear on ClocklessIdf5's forward edges.
            sym_calls(
                "ClocklessIdf5_helper",
                500,
                Some("libFastLED.a"),
                "clockless_idf5.cpp.o",
                vec!["fl::sort"],
            ),
            sym(
                "esp_log_write",
                "esp_log_write",
                2_000,
                Some("libesp.a"),
                "log.c.o",
                Vec::new(),
            ),
            sym(
                "fl::sort",
                "fl::sort",
                1_500,
                Some("libFastLED.a"),
                "sort.cpp.o",
                Vec::new(),
            ),
        ];
        let cfg = GraphConfig {
            direction: Direction::Forward,
            collapse_archives: Vec::new(),
            ..GraphConfig::default()
        };
        let g = BackrefGraph::build(&map(symbols), "ClocklessIdf5", &cfg);
        let callees: Vec<&str> = g
            .nodes
            .iter()
            .filter_map(|n| match &n.kind {
                NodeKind::Callee { demangled, .. } => Some(demangled.as_str()),
                _ => None,
            })
            .collect();
        assert!(callees.contains(&"esp_log_write"));
        assert!(
            !callees.contains(&"fl::sort"),
            "fl::sort is a sibling's callee, not the root's; got callees: {callees:?}"
        );
    }

    /// Bidirectional: root has BOTH a TU caller (backref) and a
    /// resolved callee (forward).
    #[test]
    fn bidirectional_walks_both_sides() {
        let symbols = vec![
            // Root: called by main.cpp.o; itself calls esp_log_write.
            {
                let mut r = sym_calls(
                    "ClocklessIdf5",
                    10_000,
                    Some("libFastLED.a"),
                    "clockless_idf5.cpp.o",
                    vec!["esp_log_write"],
                );
                r.referenced_by = vec![SymbolReference {
                    archive: None,
                    object: "main.cpp.o".to_string(),
                }];
                r
            },
            sym(
                "esp_log_write",
                "esp_log_write",
                2_000,
                Some("libesp.a"),
                "log.c.o",
                Vec::new(),
            ),
            sym("setup", "setup", 30, None, "main.cpp.o", Vec::new()),
        ];
        let cfg = GraphConfig {
            direction: Direction::Bidirectional,
            collapse_archives: Vec::new(),
            ..GraphConfig::default()
        };
        let g = BackrefGraph::build(&map(symbols), "ClocklessIdf5", &cfg);
        // Edges must include at least one of each direction.
        let backward_count = g
            .edges
            .iter()
            .filter(|e| e.direction == EdgeDirection::Backward)
            .count();
        let forward_count = g
            .edges
            .iter()
            .filter(|e| e.direction == EdgeDirection::Forward)
            .count();
        assert!(backward_count >= 1, "expected at least 1 backward edge");
        assert!(forward_count >= 1, "expected at least 1 forward edge");
    }

    /// Forward fan-out cap: 10 callees, cap = 3 → 3 callee nodes + 1
    /// `(… and 7 more callees)` super-node.
    #[test]
    fn forward_fan_out_overflows_collapse() {
        let mut callees: Vec<&'static str> = Vec::new();
        let leak: Vec<String> = (0..10).map(|i| format!("callee_{i}")).collect();
        let leaked: Vec<&'static str> = leak
            .iter()
            .map(|s| Box::leak(s.clone().into_boxed_str()) as &'static str)
            .collect();
        for c in &leaked {
            callees.push(c);
        }
        let mut symbols = vec![sym_calls(
            "hub",
            1_000,
            Some("libcore.a"),
            "core.o",
            callees,
        )];
        // Give each callee a distinct size so ranking is deterministic.
        for (i, name) in leaked.iter().enumerate() {
            symbols.push(sym(
                name,
                name,
                (100 - i) as u64,
                Some("libapp.a"),
                &format!("{name}.o"),
                Vec::new(),
            ));
        }
        let cfg = GraphConfig {
            direction: Direction::Forward,
            fan_out: 3,
            collapse_archives: Vec::new(),
            ..GraphConfig::default()
        };
        let g = BackrefGraph::build(&map(symbols), "hub", &cfg);
        let callee_nodes = g
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, NodeKind::Callee { .. }))
            .count();
        assert_eq!(callee_nodes, 3, "expected exactly 3 callee nodes after cap");
        let overflow = g.nodes.iter().find(
            |n| matches!(&n.kind, NodeKind::Collapsed { archive, .. } if archive == "(overflow)"),
        );
        assert!(overflow.is_some(), "expected an overflow super-node");
        assert!(overflow.unwrap().label.contains("7 more callees"));
    }

    /// Forward edges in the .dot output must be styled distinctly
    /// (dashed + blue) so a bidirectional graph stays readable.
    #[test]
    fn forward_edges_styled_in_dot() {
        let symbols = vec![
            sym_calls("a", 100, Some("libA.a"), "a.o", vec!["b"]),
            sym("b", "b", 50, Some("libB.a"), "b.o", Vec::new()),
        ];
        let cfg = GraphConfig {
            direction: Direction::Forward,
            collapse_archives: Vec::new(),
            ..GraphConfig::default()
        };
        let g = BackrefGraph::build(&map(symbols), "a", &cfg);
        let dot = g.to_dot();
        assert!(
            dot.contains("style=dashed"),
            "forward edge missing dashed style"
        );
        assert!(
            dot.contains("label=\"calls\""),
            "forward edge missing label"
        );
    }

    /// `rank_callees_dual` returns top-N by both size and popularity,
    /// plus an "other" count that excludes both top-N pickups.
    #[test]
    fn rank_callees_dual_separates_size_and_popularity() {
        // Caller has 5 callees:
        //   a: size=1000, shared by 1 caller   (heavy, unpopular)
        //   b: size=500,  shared by 10 callers (medium, popular)
        //   c: size=300,  shared by 3 callers
        //   d: size=200,  shared by 2 callers
        //   e: size=100,  shared by 1 caller
        let symbols = vec![
            sym_calls(
                "root",
                10_000,
                None,
                "main.o",
                vec!["a", "b", "c", "d", "e"],
            ),
            {
                let mut s = sym("a", "a", 1_000, Some("libX.a"), "a.o", Vec::new());
                s.referenced_by = vec![SymbolReference {
                    archive: None,
                    object: "x.o".to_string(),
                }];
                s
            },
            {
                let mut s = sym("b", "b", 500, Some("libX.a"), "b.o", Vec::new());
                s.referenced_by = (0..10)
                    .map(|i| SymbolReference {
                        archive: None,
                        object: format!("caller_{i}.o"),
                    })
                    .collect();
                s
            },
            {
                let mut s = sym("c", "c", 300, Some("libX.a"), "c.o", Vec::new());
                s.referenced_by = (0..3)
                    .map(|i| SymbolReference {
                        archive: None,
                        object: format!("caller_{i}.o"),
                    })
                    .collect();
                s
            },
            {
                let mut s = sym("d", "d", 200, Some("libX.a"), "d.o", Vec::new());
                s.referenced_by = (0..2)
                    .map(|i| SymbolReference {
                        archive: None,
                        object: format!("caller_{i}.o"),
                    })
                    .collect();
                s
            },
            sym("e", "e", 100, Some("libX.a"), "e.o", Vec::new()),
        ];
        let m = map(symbols);
        let root_sym = m.symbols.iter().find(|s| s.mangled == "root").unwrap();
        let (by_size, by_pop, other) = rank_callees_dual(&m, root_sym, 3);
        // By size: a > b > c.
        assert_eq!(
            by_size
                .iter()
                .map(|c| c.mangled.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
        // By popularity: b (10) > c (3) > d (2).
        assert_eq!(
            by_pop
                .iter()
                .map(|c| c.mangled.as_str())
                .collect::<Vec<_>>(),
            vec!["b", "c", "d"]
        );
        // Other = unique callees not in either bucket. Top picks
        // union: {a, b, c, d}. Total: {a, b, c, d, e}. Other: {e}.
        assert_eq!(other, 1, "expected exactly e in the 'other' bucket");
    }

    /// Default config still gives the pre-#471 backward-only
    /// behaviour: no callee nodes, no forward edges.
    #[test]
    fn default_direction_is_backward_only() {
        let symbols = vec![
            sym_calls(
                "ClocklessIdf5",
                10_000,
                Some("libFastLED.a"),
                "clockless_idf5.cpp.o",
                vec!["esp_log_write"],
            ),
            sym(
                "esp_log_write",
                "esp_log_write",
                2_000,
                Some("libesp.a"),
                "log.c.o",
                Vec::new(),
            ),
        ];
        let g = BackrefGraph::build(&map(symbols), "ClocklessIdf5", &GraphConfig::default());
        assert!(g
            .nodes
            .iter()
            .all(|n| !matches!(n.kind, NodeKind::Callee { .. })));
        assert!(g
            .edges
            .iter()
            .all(|e| e.direction == EdgeDirection::Backward));
    }
}
