use std::collections::BTreeSet;

use super::*;
use crate::symbol_analysis::{
    FineGrainedSymbol, FineGrainedSymbolMap, SectionBytes, SymbolReference,
};
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
