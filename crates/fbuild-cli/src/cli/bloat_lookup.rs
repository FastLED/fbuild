//! `fbuild bloat lookup` — single-symbol bloat-metric query.
//!
//! Resolves one demangled or mangled symbol against the full
//! fine-grained symbol map, and prints a focused per-symbol block
//! (size, archive, object, region, per-symbol callers, per-symbol
//! callees, TU-level referencers). Designed for AI optimisation
//! passes that have a specific symbol in mind and want its row
//! without rendering the entire top-N report.

use std::path::PathBuf;

use fbuild_build::symbol_analyzer::{
    analyze_elf, default_map_path, discover_elf_in_project, AnalyzeConfig,
};
use fbuild_core::symbol_analysis::{
    FineGrainedSymbol, FineGrainedSymbolMap, SymbolLookup, SymbolQuery,
};
use fbuild_core::{FbuildError, Result};

use super::symbols_cmd::resolve_tool_paths_public;

#[allow(clippy::too_many_arguments)]
pub fn run_bloat_lookup(
    input: String,
    symbol: Option<String>,
    symbol_mangled: Option<String>,
    json: bool,
    map: Option<String>,
    nm: Option<String>,
    cppfilt: Option<String>,
    build_info: Option<String>,
) -> Result<()> {
    if symbol.is_none() && symbol_mangled.is_none() {
        return Err(FbuildError::BuildFailed(
            "lookup requires one of --symbol or --symbol-mangled".into(),
        ));
    }

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
    )?;

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
    let report = analyze_elf(cfg)?;

    let query_str = symbol.clone().or_else(|| symbol_mangled.clone()).unwrap();
    let query = match (&symbol, &symbol_mangled) {
        (Some(s), _) => SymbolQuery::SubstringDemangled(s),
        (None, Some(m)) => SymbolQuery::ExactMangled(m),
        (None, None) => unreachable!("guarded above"),
    };

    match report.find_symbol(&query) {
        SymbolLookup::Hit(sym) => {
            if json {
                let out = serde_json::to_string_pretty(sym)
                    .map_err(|e| FbuildError::Other(format!("json serialize: {e}")))?;
                println!("{out}");
            } else {
                print!("{}", format_symbol_block(sym, &report));
            }
            Ok(())
        }
        SymbolLookup::Ambiguous(candidates) => {
            if json {
                let payload: Vec<&FineGrainedSymbol> = candidates;
                let out = serde_json::to_string_pretty(&payload)
                    .map_err(|e| FbuildError::Other(format!("json serialize: {e}")))?;
                println!("{out}");
            } else {
                eprintln!(
                    "ambiguous: {} symbols match `{}`:",
                    candidates.len(),
                    query_str
                );
                for c in &candidates {
                    eprintln!("  {} B  {}", c.size, c.demangled);
                }
                eprintln!("re-run with a more specific --symbol value (or use --symbol-mangled).");
            }
            Err(FbuildError::BuildFailed(format!(
                "ambiguous symbol query `{query_str}`"
            )))
        }
        SymbolLookup::Miss => Err(FbuildError::BuildFailed(format!(
            "no symbol matched `{query_str}` (try a substring of the demangled name)"
        ))),
    }
}

/// Render the human-readable per-symbol block for `fbuild bloat lookup`.
/// Shows the row's facts, then per-symbol callers (called_by, objdump),
/// per-symbol callees (references_to, objdump), and TU-level referencers
/// (referenced_by, cref).
fn format_symbol_block(sym: &FineGrainedSymbol, map: &FineGrainedSymbolMap) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "{}", sym.demangled);
    let _ = writeln!(out, "  size:        {} B  {:?}", sym.size, sym.region);
    if !sym.mangled.is_empty() && sym.mangled != sym.demangled {
        let _ = writeln!(out, "  mangled:     {}", sym.mangled);
    }
    if let Some(a) = &sym.archive {
        let _ = writeln!(out, "  archive:     {a}");
    }
    if let Some(o) = &sym.object {
        let _ = writeln!(out, "  object:      {o}");
    }
    if let Some(s) = &sym.output_section {
        let _ = writeln!(out, "  section:     {s}");
    }
    let _ = writeln!(out, "  address:     {:#x}", sym.address);
    let _ = writeln!(out, "  source:      {}", sym.source);
    let _ = writeln!(out);

    // Per-symbol callers (called_by) — the #478 use case.
    if !sym.called_by.is_empty() {
        let _ = writeln!(
            out,
            "  Callers (per-symbol, {} direct):",
            sym.called_by.len()
        );
        let mut rows: Vec<(u64, &str, &str)> = sym
            .called_by
            .iter()
            .map(|m| {
                let resolved = map
                    .symbols
                    .iter()
                    .find(|r| r.mangled == *m || r.demangled == *m);
                let size = resolved.map(|r| r.size).unwrap_or(0);
                let demangled = resolved.map(|r| r.demangled.as_str()).unwrap_or(m.as_str());
                (size, demangled, m.as_str())
            })
            .collect();
        rows.sort_by(|a, b| b.0.cmp(&a.0));
        for (size, demangled, _) in rows.iter().take(10) {
            let _ = writeln!(out, "    {size:>8} B  {demangled}");
        }
        if rows.len() > 10 {
            let _ = writeln!(out, "    … and {} more callers", rows.len() - 10);
        }
        let _ = writeln!(out);
    }

    // Per-symbol callees (references_to).
    if !sym.references_to.is_empty() {
        let _ = writeln!(
            out,
            "  Callees (per-symbol, {} direct):",
            sym.references_to.len()
        );
        let mut rows: Vec<(u64, &str)> = sym
            .references_to
            .iter()
            .map(|m| {
                let resolved = map
                    .symbols
                    .iter()
                    .find(|r| r.mangled == *m || r.demangled == *m);
                let size = resolved.map(|r| r.size).unwrap_or(0);
                let demangled = resolved.map(|r| r.demangled.as_str()).unwrap_or(m.as_str());
                (size, demangled)
            })
            .collect();
        rows.sort_by(|a, b| b.0.cmp(&a.0));
        for (size, demangled) in rows.iter().take(10) {
            let _ = writeln!(out, "    {size:>8} B  {demangled}");
        }
        if rows.len() > 10 {
            let _ = writeln!(out, "    … and {} more callees", rows.len() - 10);
        }
        let _ = writeln!(out);
    }

    // TU-level referencers (referenced_by, cref).
    if !sym.referenced_by.is_empty() {
        let _ = writeln!(
            out,
            "  Referenced by ({} TUs, cref-derived):",
            sym.referenced_by.len()
        );
        for r in sym.referenced_by.iter().take(10) {
            let label = match &r.archive {
                Some(a) => format!("{a}({})", r.object),
                None => r.object.clone(),
            };
            let _ = writeln!(out, "    {label}");
        }
        if sym.referenced_by.len() > 10 {
            let _ = writeln!(
                out,
                "    … and {} more referencers",
                sym.referenced_by.len() - 10
            );
        }
    }
    out
}
