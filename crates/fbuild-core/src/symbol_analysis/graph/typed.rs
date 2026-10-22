//! Traversal of address-qualified final-image evidence, without name-only joins.
use super::*;
use crate::symbol_analysis::{
    AnalysisStatus, FineGrainedSymbol, ReferenceEdge, ReferenceKind, SymbolIdentity,
};

pub(super) fn available(map: &FineGrainedSymbolMap) -> bool {
    map.reference_analysis.disassembly.status == AnalysisStatus::Analyzed
        || map.reference_analysis.static_data.status == AnalysisStatus::Analyzed
}

fn node_id(identity: &SymbolIdentity) -> String {
    let encoded: String = identity
        .name
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!(
        "sym__{encoded}__{:x}__{}",
        identity.address,
        sanitize_id(&identity.source)
    )
}

fn edge_direction(kind: &ReferenceKind) -> EdgeDirection {
    match kind {
        ReferenceKind::Disassembly => EdgeDirection::InstructionReference,
        ReferenceKind::StaticData => EdgeDirection::StaticPointer,
        ReferenceKind::FragmentOwner => EdgeDirection::FragmentOwner,
    }
}

struct Candidate<'a> {
    identity: SymbolIdentity,
    symbol: Option<&'a FineGrainedSymbol>,
    edges: Vec<&'a ReferenceEdge>,
}
impl Candidate<'_> {
    fn size(&self) -> u64 {
        self.symbol.map_or(0, |s| s.size)
    }
    fn archive(&self) -> Option<&str> {
        self.symbol.and_then(|s| s.archive.as_deref())
    }
}

fn candidates<'a>(
    index: &TuIndex<'a>,
    identity: &SymbolIdentity,
    incoming: bool,
    config: &GraphConfig,
) -> Vec<Candidate<'a>> {
    let adjacency = if incoming {
        &index.incoming
    } else {
        &index.outgoing
    };
    let mut grouped = BTreeMap::<SymbolIdentity, Vec<&ReferenceEdge>>::new();
    for edge in adjacency.get(identity).into_iter().flatten() {
        let target = if incoming { &edge.source } else { &edge.target };
        grouped.entry(target.clone()).or_default().push(*edge);
    }
    let mut candidates: Vec<_> = grouped
        .into_iter()
        .map(|(identity, edges)| {
            let symbol = index.by_identity.get(&identity).copied();
            Candidate {
                identity,
                symbol,
                edges,
            }
        })
        .filter(|c| {
            !c.archive()
                .is_some_and(|a| config.exclude_archives.iter().any(|x| x == a))
        })
        .collect();
    candidates.sort_by(|a, b| {
        b.size()
            .cmp(&a.size())
            .then_with(|| a.identity.cmp(&b.identity))
    });
    candidates
}

struct Walker<'a, 'b> {
    index: &'b TuIndex<'a>,
    config: &'b GraphConfig,
    root_archive: Option<String>,
    graph: BackrefGraph,
    nodes: BTreeSet<String>,
    expanded: BTreeSet<(SymbolIdentity, bool)>,
    queue: VecDeque<(SymbolIdentity, bool, u32)>,
}
impl<'a, 'b> Walker<'a, 'b> {
    fn expand(&mut self, identity: SymbolIdentity, incoming: bool, depth: u32) {
        let limit = match self.config.depth {
            GraphDepth::Fixed(n) => n.min(self.config.max_depth),
            GraphDepth::Adaptive => self.config.max_depth,
        };
        if depth >= limit || !self.expanded.insert((identity.clone(), incoming)) {
            return;
        }
        let current = node_id(&identity);
        let candidates = candidates(self.index, &identity, incoming, self.config);
        let mut collapsed = BTreeMap::<String, Vec<&Candidate<'_>>>::new();
        let mut remaining = Vec::new();
        for c in &candidates {
            if let Some(archive) = c
                .archive()
                .filter(|a| self.config.collapse_archives.iter().any(|x| x == a))
            {
                collapsed.entry(archive.to_string()).or_default().push(c);
                continue;
            }
            remaining.push(c);
        }
        for c in remaining.iter().take(self.config.fan_out) {
            let id = node_id(&c.identity);
            if self.nodes.insert(id.clone()) {
                self.graph.nodes.push(GraphNode {
                    id: id.clone(),
                    label: format!(
                        "{}\n{} B\n0x{:x} · {}",
                        c.symbol
                            .map_or(c.identity.name.as_str(), |s| s.demangled.as_str()),
                        c.size(),
                        c.identity.address,
                        c.identity.source
                    ),
                    archive: c.symbol.and_then(|s| s.archive.clone()),
                    object: c.symbol.and_then(|s| s.object.clone()),
                    kind: NodeKind::ReferenceSymbol { size: c.size() },
                    depth: depth + 1,
                });
            }
            self.connect(&current, &id, incoming, &c.edges);
            let crosses_archive =
                self.root_archive.is_some() && c.archive() != self.root_archive.as_deref();
            if !matches!(self.config.depth, GraphDepth::Adaptive) || !crosses_archive {
                self.queue
                    .push_back((c.identity.clone(), incoming, depth + 1));
            }
        }
        for (archive, members) in collapsed {
            let id = format!(
                "typed_collapse__{current}__{}__{incoming}",
                sanitize_id(&archive)
            );
            if self.nodes.insert(id.clone()) {
                self.graph.nodes.push(GraphNode {
                    id: id.clone(),
                    label: format!("{archive}\n{} references", members.len()),
                    archive: Some(archive.clone()),
                    object: None,
                    kind: NodeKind::Collapsed {
                        archive,
                        count: members.len(),
                    },
                    depth: depth + 1,
                });
            }
            for c in members {
                self.connect(&current, &id, incoming, &c.edges);
            }
        }
        let overflow = remaining.len().saturating_sub(self.config.fan_out);
        if overflow > 0 {
            let id = format!("typed_overflow__{current}__{incoming}");
            self.graph.nodes.push(GraphNode {
                id: id.clone(),
                label: format!("(… and {overflow} more references)"),
                archive: None,
                object: None,
                kind: NodeKind::Collapsed {
                    archive: "(overflow)".into(),
                    count: overflow,
                },
                depth: depth + 1,
            });
            self.graph.edges.push(if incoming {
                GraphEdge::backward(id, current)
            } else {
                // Overflow may mix static pointers and instruction references;
                // an unqualified arrow must never claim runtime calls.
                GraphEdge::backward(current, id)
            });
        }
    }
    fn connect(&mut self, current: &str, other: &str, incoming: bool, evidence: &[&ReferenceEdge]) {
        for e in evidence {
            let (from, to) = if incoming {
                (other, current)
            } else {
                (current, other)
            };
            let edge = GraphEdge {
                from: from.into(),
                to: to.into(),
                direction: edge_direction(&e.kind),
            };
            if !self.graph.edges.contains(&edge) {
                self.graph.edges.push(edge);
            }
        }
    }
}

pub(super) fn build(
    index: &TuIndex<'_>,
    root: &FineGrainedSymbol,
    config: &GraphConfig,
) -> BackrefGraph {
    let identity = SymbolIdentity::from(root);
    let id = node_id(&identity);
    let root_node = GraphNode {
        id: id.clone(),
        label: format!(
            "{}\n{} B\n0x{:x} · {}",
            root.demangled, root.size, root.address, root.source
        ),
        archive: root.archive.clone(),
        object: root.object.clone(),
        kind: NodeKind::RootSymbol {
            demangled: root.demangled.clone(),
            size: root.size,
        },
        depth: 0,
    };
    let mut walker = Walker {
        index,
        config,
        root_archive: root.archive.clone(),
        graph: BackrefGraph {
            root_id: id.clone(),
            nodes: vec![root_node],
            edges: vec![],
        },
        nodes: BTreeSet::from([id]),
        expanded: BTreeSet::new(),
        queue: VecDeque::new(),
    };
    if matches!(
        config.direction,
        Direction::Backward | Direction::Bidirectional
    ) {
        walker.queue.push_back((identity.clone(), true, 0));
    }
    if matches!(
        config.direction,
        Direction::Forward | Direction::Bidirectional
    ) {
        walker.queue.push_back((identity, false, 0));
    }
    while let Some((identity, incoming, depth)) = walker.queue.pop_front() {
        walker.expand(identity, incoming, depth);
    }
    add_objects(&mut walker.graph, index, root, config);
    walker.graph
}

fn add_objects(
    graph: &mut BackrefGraph,
    index: &TuIndex<'_>,
    root: &FineGrainedSymbol,
    config: &GraphConfig,
) {
    let limit = match config.depth {
        GraphDepth::Fixed(n) => n.min(config.max_depth),
        GraphDepth::Adaptive => config.max_depth,
    };
    if config.direction == Direction::Forward || limit == 0 {
        return;
    }
    let mut nodes = BTreeSet::new();
    let mut queue = VecDeque::from([(root.referenced_by.clone(), graph.root_id.clone(), 1)]);
    while let Some((references, target, depth)) = queue.pop_front() {
        for reference in rank_and_cap_referencers(&references, index, config, &root.archive, depth)
        {
            let (node, tu) = object_node(reference, index, &target, depth);
            let id = node.id.clone();
            let fresh = nodes.insert(id.clone());
            if fresh {
                graph.nodes.push(node);
            }
            let edge = GraphEdge {
                from: id.clone(),
                to: target.clone(),
                direction: EdgeDirection::ObjectReference,
            };
            if edge.from != edge.to && !graph.edges.contains(&edge) {
                graph.edges.push(edge);
            }
            let Some(tu) = tu else { continue };
            let crosses_archive = root.archive.is_some() && tu.archive != root.archive;
            if !fresh
                || depth >= limit
                || (matches!(config.depth, GraphDepth::Adaptive) && crosses_archive)
            {
                continue;
            }
            let mut seen = BTreeSet::new();
            let mut parents = Vec::new();
            for symbol in index.symbols_in(&tu) {
                for parent in &symbol.referenced_by {
                    let key = (parent.archive.clone(), parent.object.clone());
                    if key != (tu.archive.clone(), tu.object.clone()) && seen.insert(key) {
                        parents.push(parent.clone());
                    }
                }
            }
            queue.push_back((parents, id, depth + 1));
        }
    }
}

fn object_node(
    reference: CappedReferencer,
    index: &TuIndex<'_>,
    target: &str,
    depth: u32,
) -> (GraphNode, Option<SymbolReference>) {
    let (identity, label, archive, object, kind, tu) = match reference {
        CappedReferencer::Tu(tu) => {
            let size = index.bytes_in(&tu);
            (
                format!("{:?}/{}", tu.archive, tu.object),
                format!("{}\nobject reference\n{size} B in TU", tu.object),
                tu.archive.clone(),
                Some(tu.object.clone()),
                NodeKind::TranslationUnit {
                    size_hint: Some(size),
                },
                Some(tu),
            )
        }
        CappedReferencer::CollapsedArchive { archive, count } => (
            format!("collapse/{target}/{archive}"),
            format!("{archive}\n{count} object references"),
            Some(archive.clone()),
            None,
            NodeKind::Collapsed { archive, count },
            None,
        ),
        CappedReferencer::FanOutOverflow { count } => (
            format!("overflow/{target}/{depth}"),
            format!("(… and {count} more object references)"),
            None,
            None,
            NodeKind::Collapsed {
                archive: "(overflow)".into(),
                count,
            },
            None,
        ),
    };
    let id = node_id(&SymbolIdentity {
        name: identity,
        address: 0,
        source: "object".into(),
    });
    (
        GraphNode {
            id,
            label,
            archive,
            object,
            kind,
            depth,
        },
        tu,
    )
}

#[cfg(test)]
mod tests {
    use super::super::tests::{map, refr, sym};
    use super::*;

    fn typed_chain(
        mut symbols: Vec<FineGrainedSymbol>,
        edges: &[(usize, usize)],
    ) -> FineGrainedSymbolMap {
        use crate::symbol_analysis::{AnalysisStatus, ReferenceEdge, ReferenceKind};
        for (i, symbol) in symbols.iter_mut().enumerate() {
            symbol.address += i as u64 * 0x100;
        }
        let mut report = map(symbols);
        report.reference_analysis.static_data.status = AnalysisStatus::Analyzed;
        for &(source, target) in edges {
            report.reference_analysis.edges.push(ReferenceEdge {
                source: (&report.symbols[source]).into(),
                target: (&report.symbols[target]).into(),
                kind: ReferenceKind::StaticData,
                offset: Some(8),
            });
        }
        report
    }

    #[test]
    fn typed_forward_overflow_never_claims_runtime_calls() {
        let report = typed_chain(
            vec![
                sym("root", "root", 100, None, "root.o", vec![]),
                sym("target", "target", 10, None, "target.o", vec![]),
            ],
            &[(0, 1)],
        );
        let config = GraphConfig {
            direction: Direction::Forward,
            fan_out: 0,
            ..Default::default()
        };
        let dot = BackrefGraph::build(&report, "root", &config).to_dot();
        assert!(dot.contains("more references"));
        assert!(!dot.contains("label=\"calls\""));
    }

    #[test]
    fn typed_collapse_precedes_fanout_and_counts_the_entire_archive() {
        let mut symbols = vec![
            sym("root", "root", 100, Some("app.a"), "root.o", vec![]),
            sym(
                "application",
                "application",
                1,
                Some("app.a"),
                "app.o",
                vec![],
            ),
        ];
        for i in 0..6 {
            symbols.push(sym(
                &format!("libc{i}"),
                &format!("libc{i}"),
                1000,
                Some("libc.a"),
                &format!("libc{i}.o"),
                vec![],
            ));
        }
        let edges: Vec<_> = (1..symbols.len()).map(|i| (i, 0)).collect();
        let report = typed_chain(symbols, &edges);
        let config = GraphConfig {
            depth: GraphDepth::Fixed(1),
            fan_out: 1,
            ..Default::default()
        };
        let graph = BackrefGraph::build(&report, "root", &config);
        assert!(
            graph
                .nodes
                .iter()
                .any(|n| n.label.starts_with("application\n"))
        );
        assert!(graph.nodes.iter().any(
            |n| matches!(&n.kind, NodeKind::Collapsed { archive, count: 6 } if archive == "libc.a")
        ));
        assert!(!graph.to_dot().contains("more references"));
    }

    #[test]
    fn typed_adaptive_bare_root_can_expand_archived_endpoints() {
        let report = typed_chain(
            vec![
                sym("root", "root", 100, None, "root.o", vec![]),
                sym("first", "first", 10, Some("driver.a"), "first.o", vec![]),
                sym("second", "second", 10, Some("driver.a"), "second.o", vec![]),
            ],
            &[(1, 0), (2, 1)],
        );
        let graph = BackrefGraph::build(&report, "root", &GraphConfig::default());
        assert_eq!(graph.nodes.len(), 3);
    }

    #[test]
    fn typed_object_references_preserve_transitive_depth_cycles_and_controls() {
        let report = typed_chain(
            vec![
                sym(
                    "root",
                    "root",
                    100,
                    Some("app.a"),
                    "root.o",
                    vec![refr(Some("app.a"), "first.o")],
                ),
                sym(
                    "first",
                    "first",
                    20,
                    Some("app.a"),
                    "first.o",
                    vec![refr(Some("app.a"), "second.o")],
                ),
                sym(
                    "second",
                    "second",
                    30,
                    Some("app.a"),
                    "second.o",
                    vec![refr(Some("app.a"), "first.o")],
                ),
            ],
            &[],
        );
        let config = GraphConfig {
            depth: GraphDepth::Fixed(3),
            collapse_archives: vec![],
            ..Default::default()
        };
        let graph = BackrefGraph::build(&report, "root", &config);
        assert_eq!(graph.nodes.len(), 3);
        assert!(
            graph
                .nodes
                .iter()
                .any(|n| n.object.as_deref() == Some("second.o") && n.depth == 2)
        );
        assert_eq!(graph.edges.len(), 3);
        let shallow = GraphConfig {
            depth: GraphDepth::Fixed(1),
            ..config.clone()
        };
        assert_eq!(
            BackrefGraph::build(&report, "root", &shallow).nodes.len(),
            2
        );
        let excluded = GraphConfig {
            exclude_archives: vec!["app.a".into()],
            ..config.clone()
        };
        assert_eq!(
            BackrefGraph::build(&report, "root", &excluded).nodes.len(),
            1
        );
        let forward = GraphConfig {
            direction: Direction::Forward,
            ..config
        };
        assert_eq!(
            BackrefGraph::build(&report, "root", &forward).nodes.len(),
            1
        );
    }
}
