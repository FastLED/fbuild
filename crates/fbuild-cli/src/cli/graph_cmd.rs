//! `fbuild bloat graph` — render a back-reference graph for one
//! symbol as Graphviz `.dot`.
//!
//! Consumes the same toolchain resolution as `fbuild symbols` (#428)
//! so callers don't have to wire `--nm` / `--cppfilt` twice. Walks
//! the symbol's transitive `referenced_by` data outward using the
//! adaptive strategy documented in
//! [`fbuild_core::symbol_analysis::graph`].
//!
//! Output goes either to `-o <path>` or stdout. The walker is pure
//! (no I/O); this module is only the CLI glue.

use std::path::PathBuf;

use fbuild_build::symbol_analyzer::{
    AnalyzeConfig, analyze_elf, default_map_path, discover_elf_in_project,
};
use fbuild_core::symbol_analysis::{BackrefGraph, GraphConfig, GraphDepth};
use fbuild_core::{FbuildError, Result};

use crate::output as cli_output;

use super::symbols_cmd::resolve_tool_paths_public;

#[allow(clippy::too_many_arguments)]
pub async fn run_bloat_graph(
    input: String,
    symbol: String,
    map: Option<String>,
    nm: Option<String>,
    cppfilt: Option<String>,
    build_info: Option<String>,
    output: Option<String>,
    depth: String,
    fan_out: usize,
    max_depth: u32,
    collapse_archive: String,
    exclude_archive: String,
) -> Result<()> {
    let input_path = PathBuf::from(&input);
    if !input_path.exists() {
        return Err(FbuildError::BuildFailed(format!(
            "input not found: {}",
            input_path.display()
        )));
    }
    let elf_path = if input_path.is_dir() {
        discover_elf_in_project(&input_path).ok_or_else(|| {
            FbuildError::BuildFailed(format!("no ELF found under {}", input_path.display()))
        })?
    } else {
        input_path
    };

    let (nm_path, cppfilt_path, objdump_path) = resolve_tool_paths_public(
        &elf_path,
        nm.as_deref(),
        cppfilt.as_deref(),
        build_info.as_deref(),
    )
    .await?;

    let map_path_owned = map
        .map(PathBuf::from)
        .or_else(|| default_map_path(&elf_path));

    let cfg = AnalyzeConfig {
        elf_path: &elf_path,
        map_path: map_path_owned.as_deref(),
        nm_path: &nm_path,
        cppfilt_path: cppfilt_path.as_deref(),
        objdump_path: objdump_path.as_deref(),
    };
    let report = analyze_elf(cfg).await?;

    let graph_config = parse_graph_config(
        &depth,
        fan_out,
        max_depth,
        &collapse_archive,
        &exclude_archive,
    )?;
    let selected = select_symbol(&report.symbols, &symbol)?;
    let graph = if let Some(root) = selected {
        let index = fbuild_core::symbol_analysis::TuIndex::build(&report);
        BackrefGraph::build_for_symbol_with_index(&report, &index, root, &graph_config)
    } else {
        BackrefGraph::build(&report, &symbol, &graph_config)
    };
    let dot = graph.to_dot();

    match output {
        Some(path) => {
            std::fs::write(&path, &dot).map_err(|e| {
                FbuildError::Io(std::io::Error::new(e.kind(), format!("write {path}: {e}")))
            })?;
            cli_output::result(format!(
                "Wrote back-reference graph for {symbol} to {path} ({} nodes, {} edges)",
                graph.nodes.len(),
                graph.edges.len()
            ));
        }
        None => {
            // `dot` is the final answer (DOT graph text) — emit verbatim. Strip
            // any trailing newline so result()'s newline doesn't double up.
            cli_output::result(dot.trim_end_matches('\n'));
        }
    }
    Ok(())
}

fn select_symbol<'a>(
    symbols: &'a [fbuild_core::symbol_analysis::FineGrainedSymbol],
    symbol: &str,
) -> Result<Option<&'a fbuild_core::symbol_analysis::FineGrainedSymbol>> {
    let (query, address) = symbol
        .rsplit_once("@0x")
        .and_then(|(name, addr)| {
            u64::from_str_radix(addr, 16)
                .ok()
                .map(|addr| (name, Some(addr)))
        })
        .unwrap_or((symbol, None));
    let mut matches: Vec<_> = symbols
        .iter()
        .filter(|s| {
            (s.mangled == query || s.demangled == query) && address.is_none_or(|a| s.address == a)
        })
        .collect();
    if address.is_none() && matches.iter().any(|s| s.source == "nm") {
        matches.retain(|s| s.source == "nm");
    }
    matches.sort_by_key(|s| (s.address, s.source.as_str(), s.mangled.as_str()));
    matches.dedup_by_key(|s| (s.address, s.source.as_str(), s.mangled.as_str()));
    if matches.len() > 1 {
        return Err(FbuildError::BuildFailed(format!(
            "Ambiguous symbol {query}; select name@0xADDRESS. Matches: {}",
            matches
                .iter()
                .map(|s| format!("0x{:x} ({})", s.address, s.source))
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    Ok(matches.first().copied())
}

/// Parse the user-facing flag strings into a fully-populated
/// [`GraphConfig`]. Public-ish helper because the report-embed path
/// in `symbols_cmd.rs` parses the same flag shapes for the
/// `--graph-*` set.
pub fn parse_graph_config(
    depth: &str,
    fan_out: usize,
    max_depth: u32,
    collapse_archive: &str,
    exclude_archive: &str,
) -> Result<GraphConfig> {
    let depth = match depth.trim().to_ascii_lowercase().as_str() {
        "adaptive" | "auto" | "" => GraphDepth::Adaptive,
        s => match s.parse::<u32>() {
            Ok(n) => GraphDepth::Fixed(n),
            Err(_) => {
                return Err(FbuildError::BuildFailed(format!(
                    "graph --depth: expected 'adaptive' or a non-negative \
                     integer, got `{s}`"
                )));
            }
        },
    };
    let collapse_archives: Vec<String> = split_archive_list(collapse_archive);
    let exclude_archives: Vec<String> = split_archive_list(exclude_archive);
    Ok(GraphConfig {
        depth,
        fan_out: fan_out.max(1),
        max_depth: max_depth.max(1),
        collapse_archives,
        exclude_archives,
        // The `fbuild bloat graph` CLI hasn't grown a `--direction`
        // flag yet; preserve the pre-#471 behaviour (backref-only).
        // A `--direction forward|both` flag is a small follow-up
        // once Phase 4 (the markdown sub-table) demonstrates the
        // forward graph is actually useful in practice.
        direction: Default::default(),
    })
}

fn split_archive_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(|x| x.trim())
        .filter(|x| !x.is_empty())
        .map(|x| x.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_graph_config_adaptive_default() {
        let c = parse_graph_config("adaptive", 5, 4, "libc.a,libgcc.a", "").unwrap();
        assert!(matches!(c.depth, GraphDepth::Adaptive));
        assert_eq!(c.fan_out, 5);
        assert_eq!(c.max_depth, 4);
        assert_eq!(c.collapse_archives, vec!["libc.a", "libgcc.a"]);
        assert!(c.exclude_archives.is_empty());
    }

    #[test]
    fn parse_graph_config_fixed_depth() {
        let c = parse_graph_config("3", 5, 4, "", "").unwrap();
        assert!(matches!(c.depth, GraphDepth::Fixed(3)));
    }

    #[test]
    fn parse_graph_config_rejects_garbage_depth() {
        let err = parse_graph_config("woof", 5, 4, "", "").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("graph --depth"), "got: {msg}");
    }

    #[test]
    fn split_archive_list_handles_spaces_and_empties() {
        let v = split_archive_list("libc.a, libgcc.a ,, libm.a");
        assert_eq!(v, vec!["libc.a", "libgcc.a", "libm.a"]);
    }

    #[test]
    fn fan_out_zero_clamps_to_one() {
        let c = parse_graph_config("adaptive", 0, 4, "", "").unwrap();
        assert_eq!(c.fan_out, 1);
    }
}

#[cfg(test)]
mod identity_tests {
    use super::select_symbol;
    use fbuild_core::MemoryRegion;
    use fbuild_core::symbol_analysis::FineGrainedSymbol;
    fn fixture(address: u64, source: &str) -> FineGrainedSymbol {
        FineGrainedSymbol {
            mangled: "function".into(),
            demangled: "function()".into(),
            address,
            size: 8,
            sym_type: 'T',
            region: MemoryRegion::Flash,
            archive: None,
            object: Some("main.o".into()),
            output_section: None,
            source: source.into(),
            referenced_by: vec![],
            references_to: vec![],
            called_by: vec![],
        }
    }
    #[test]
    fn exact_selector_keeps_fragment_identity_and_rejects_ambiguous_functions() {
        let symbols = vec![fixture(100, "map"), fixture(200, "nm")];
        assert_eq!(
            select_symbol(&symbols, "function")
                .unwrap()
                .unwrap()
                .address,
            200
        );
        assert_eq!(
            select_symbol(&symbols, "function@0x64")
                .unwrap()
                .unwrap()
                .source,
            "map"
        );
        assert!(select_symbol(&symbols, "missing").unwrap().is_none());
        let symbols = vec![fixture(200, "nm"), fixture(300, "nm")];
        assert!(
            select_symbol(&symbols, "function")
                .unwrap_err()
                .to_string()
                .contains("Ambiguous symbol")
        );
        assert_eq!(
            select_symbol(&symbols, "function()@0x12c")
                .unwrap()
                .unwrap()
                .address,
            300
        );
    }
}
