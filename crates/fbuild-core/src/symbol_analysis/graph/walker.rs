//! Walker internals + callee ranking for [`super::BackrefGraph`].
//!
//! Lives in its own file to keep `mod.rs` under the per-file LOC cap.
//! The BFS driver in `mod.rs` calls into the helpers below; the only
//! item from this module re-exported publicly is
//! [`rank_callees_dual`] (+ its return row [`CalleeRanked`]) — those
//! preserve the historical `graph::rank_callees_dual` import path.

use std::collections::BTreeSet;

use super::{
    sanitize_id, EdgeDirection, FineGrainedSymbolMap, GraphConfig, GraphEdge, GraphNode, NodeKind,
    SymbolReference, TuIndex,
};

/// Result of ranking and capping a single layer of referencers.
pub(super) enum CappedReferencer {
    Tu(SymbolReference),
    CollapsedArchive { archive: String, count: usize },
    FanOutOverflow { count: usize },
}

/// Apply `exclude_archives`, `collapse_archives`, and the fan-out cap.
/// Returns a vector of survivors / collapsed buckets / overflow.
pub(super) fn rank_and_cap_referencers(
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
    let mut by_collapse: std::collections::BTreeMap<String, Vec<SymbolReference>> =
        std::collections::BTreeMap::new();
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

pub(super) fn push_edge_dedup(edges: &mut Vec<GraphEdge>, candidate: GraphEdge) {
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
pub(super) fn walk_forward(
    map: &FineGrainedSymbolMap,
    config: &GraphConfig,
    caller_sym: &super::super::FineGrainedSymbol,
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
            GraphEdge {
                from: caller_node_id.to_string(),
                to: callee_id,
                direction: EdgeDirection::Forward,
            },
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
        edges.push(GraphEdge {
            from: caller_node_id.to_string(),
            to: overflow_id,
            direction: EdgeDirection::Forward,
        });
    }
}

/// Working struct for ranking callees: carries the mangled name plus
/// an optional resolved symbol pointer for size/popularity lookup.
pub(super) struct CalleeCandidate<'a> {
    mangled: String,
    sym: Option<&'a super::super::FineGrainedSymbol>,
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

/// Per-caller node label: `<demangled>\n<size> B\ncalls ×N`.
/// Distinct from `format_callee_label` so users reading the rendered
/// graph can tell at a glance whether a node is a back-edge (a thing
/// calling the root) or a forward-edge (a thing the root calls).
fn format_caller_label(demangled: &str, size: u64, callees_count: usize) -> String {
    let truncated = if demangled.len() > 64 {
        format!("{}…", &demangled[..63])
    } else {
        demangled.to_string()
    };
    if callees_count > 1 {
        format!("{truncated}\n{size} B\ncalls ×{callees_count}")
    } else {
        format!("{truncated}\n{size} B")
    }
}

/// BFS the per-symbol backward edges (`called_by`) outward from a
/// starting symbol. Mirror of [`walk_forward`] but for the inverse
/// direction: surfaces "*which symbols call into this?*" with
/// per-symbol precision instead of the TU-level granularity the
/// `referenced_by` (cref-based) walker provides.
///
/// Ranking is by caller flash size descending. Same fan-out cap and
/// `max_depth` policy as `walk_forward`; same overflow super-node
/// rendering.
#[allow(clippy::too_many_arguments)]
pub(super) fn walk_backward_per_symbol(
    map: &FineGrainedSymbolMap,
    config: &GraphConfig,
    callee_sym: &super::super::FineGrainedSymbol,
    callee_node_id: &str,
    depth: u32,
    nodes: &mut Vec<GraphNode>,
    edges: &mut Vec<GraphEdge>,
    visited_syms: &mut BTreeSet<String>,
) {
    if depth > config.max_depth {
        return;
    }
    let mut callers: Vec<CallerCandidate<'_>> = Vec::new();
    for caller_mangled in &callee_sym.called_by {
        if visited_syms.contains(caller_mangled) {
            continue;
        }
        let resolved = map
            .symbols
            .iter()
            .find(|s| s.mangled == *caller_mangled || s.demangled == *caller_mangled);
        callers.push(CallerCandidate {
            mangled: caller_mangled.clone(),
            sym: resolved,
        });
    }
    if callers.is_empty() {
        return;
    }
    callers.retain(|c| match c.sym.and_then(|s| s.archive.as_ref()) {
        Some(a) => !config.exclude_archives.iter().any(|x| x == a),
        None => true,
    });
    if callers.is_empty() {
        return;
    }
    callers.sort_by(|a, b| {
        b.size()
            .cmp(&a.size())
            .then_with(|| b.callees_count().cmp(&a.callees_count()))
            .then_with(|| a.mangled.cmp(&b.mangled))
    });

    let total = callers.len();
    let take_n = config.fan_out.min(total);
    let overflow = total.saturating_sub(take_n);

    for c in callers.iter().take(take_n) {
        let caller_id = format!("sym__{}", sanitize_id(&c.mangled));
        if visited_syms.insert(c.mangled.clone()) {
            let (demangled, size) = match c.sym {
                Some(s) => (s.demangled.clone(), s.size),
                None => (c.mangled.clone(), 0),
            };
            let callees_count = c.callees_count();
            let archive = c.sym.and_then(|s| s.archive.clone());
            let object = c.sym.and_then(|s| s.object.clone());
            nodes.push(GraphNode {
                id: caller_id.clone(),
                label: format_caller_label(&demangled, size, callees_count),
                archive,
                object,
                kind: NodeKind::Caller {
                    demangled,
                    size,
                    callees_count,
                },
                depth,
            });
            if let Some(resolved) = c.sym {
                if depth < config.max_depth && !resolved.called_by.is_empty() {
                    walk_backward_per_symbol(
                        map,
                        config,
                        resolved,
                        &caller_id,
                        depth + 1,
                        nodes,
                        edges,
                        visited_syms,
                    );
                }
            }
        }
        // Backward edge: caller → callee (root). Solid arrow matches
        // the historical pre-#478 backref-edge style so existing `.dot`
        // consumers see the same visual.
        push_edge_dedup(
            edges,
            GraphEdge {
                from: caller_id,
                to: callee_node_id.to_string(),
                direction: EdgeDirection::Backward,
            },
        );
    }

    if overflow > 0 {
        let overflow_id = format!(
            "bwd_ovf__{}__d{}__{}",
            sanitize_id(callee_node_id),
            depth,
            overflow
        );
        nodes.push(GraphNode {
            id: overflow_id.clone(),
            label: format!("(… and {overflow} more callers)"),
            archive: None,
            object: None,
            kind: NodeKind::Collapsed {
                archive: "(overflow)".to_string(),
                count: overflow,
            },
            depth,
        });
        edges.push(GraphEdge {
            from: overflow_id,
            to: callee_node_id.to_string(),
            direction: EdgeDirection::Backward,
        });
    }
}

/// Working struct for ranking per-symbol callers (mirror of
/// [`CalleeCandidate`]).
pub(super) struct CallerCandidate<'a> {
    mangled: String,
    sym: Option<&'a super::super::FineGrainedSymbol>,
}

impl<'a> CallerCandidate<'a> {
    fn size(&self) -> u64 {
        self.sym.map(|s| s.size).unwrap_or(0)
    }

    fn callees_count(&self) -> usize {
        self.sym.map(|s| s.references_to.len()).unwrap_or(0)
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
    caller: &super::super::FineGrainedSymbol,
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
    pub sym: Option<&'a super::super::FineGrainedSymbol>,
}

/// Dual-axis ranking of a symbol's per-symbol callers (mirror of
/// [`rank_callees_dual`]). Returns `(by_size, by_breadth, other_count)`:
///   - `by_size`: heaviest callers (flash bytes) — answers "which big
///     thing dragged this in?" so an AI pass that wants to shrink the
///     binary knows which caller, if eliminated, would let the
///     dead-code pass also strip the callee.
///   - `by_breadth`: callers with the most callees of their own —
///     proxy for "how much else did pulling this caller in cost?" so
///     the AI can see whether eliminating a caller has high downstream
///     leverage.
///   - `other_count`: number of unique callers that fell out of both
///     top-N buckets.
///
/// Callers that don't resolve to a row in `map` get `size=0`/`callees_count=0`
/// and contribute to neither axis.
#[must_use]
pub fn rank_callers_dual<'a>(
    map: &'a FineGrainedSymbolMap,
    callee: &super::super::FineGrainedSymbol,
    top_n: usize,
) -> (Vec<CallerRanked<'a>>, Vec<CallerRanked<'a>>, usize) {
    let mut resolved: Vec<CallerRanked<'a>> = callee
        .called_by
        .iter()
        .map(|m| {
            let s = map
                .symbols
                .iter()
                .find(|s| s.mangled == *m || s.demangled == *m);
            CallerRanked {
                mangled: m.clone(),
                demangled: s.map(|s| s.demangled.clone()).unwrap_or_else(|| m.clone()),
                size: s.map(|s| s.size).unwrap_or(0),
                callees_count: s.map(|s| s.references_to.len()).unwrap_or(0),
                sym: s,
            }
        })
        .collect();

    let mut by_size = resolved.clone();
    by_size.sort_by(|a, b| {
        b.size
            .cmp(&a.size)
            .then_with(|| a.demangled.cmp(&b.demangled))
    });
    let mut by_breadth = resolved.clone();
    by_breadth.sort_by(|a, b| {
        b.callees_count
            .cmp(&a.callees_count)
            .then_with(|| b.size.cmp(&a.size))
            .then_with(|| a.demangled.cmp(&b.demangled))
    });

    let size_top = by_size.iter().take(top_n).cloned().collect::<Vec<_>>();
    let breadth_top = by_breadth.iter().take(top_n).cloned().collect::<Vec<_>>();

    let top_mangled: BTreeSet<&str> = size_top
        .iter()
        .chain(breadth_top.iter())
        .map(|c| c.mangled.as_str())
        .collect();
    let other_count = resolved
        .drain(..)
        .filter(|c| !top_mangled.contains(c.mangled.as_str()))
        .count();

    (size_top, breadth_top, other_count)
}

/// Row in the dual-ranked caller table.
#[derive(Debug, Clone)]
pub struct CallerRanked<'a> {
    pub mangled: String,
    pub demangled: String,
    pub size: u64,
    pub callees_count: usize,
    pub sym: Option<&'a super::super::FineGrainedSymbol>,
}
