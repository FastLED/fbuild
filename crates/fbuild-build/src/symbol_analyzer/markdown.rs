//! Markdown / sidecar-graph rendering for
//! [`crate::symbol_analyzer`]. Split out of `mod.rs` to keep both
//! files under the 1000-LOC CI gate; the public surface
//! (`format_markdown_report`, `format_markdown_report_with_graphs`,
//! `write_sidecar_dot_files`, `MarkdownGraphOptions`,
//! `SidecarOptions`) is re-exported from the parent module for
//! back-compat.

use std::path::Path;

use fbuild_core::symbol_analysis::graph::{rank_callees_dual, rank_callers_dual, Direction};
use fbuild_core::symbol_analysis::{
    sanitize_filename, BackrefGraph, FineGrainedSymbolMap, GraphConfig, TuIndex,
};
use fbuild_core::{FbuildError, Result};

/// Knobs controlling whether `format_markdown_report` embeds inline
/// Graphviz `.dot` blocks for the top symbols and how the walker
/// behaves while building them. `enabled = false` reproduces the
/// pre-#463 markdown shape byte-for-byte.
#[derive(Debug, Clone)]
pub struct MarkdownGraphOptions {
    /// Embed `.dot` fenced blocks under `<details>` summaries for
    /// the top symbols.
    pub enabled: bool,
    /// How many top symbols (by flash bytes) get an embedded graph.
    /// Capped further by the report's own `top_n`.
    pub graph_top: usize,
    /// Walker / rendering configuration for each embedded graph.
    pub config: GraphConfig,
}

impl Default for MarkdownGraphOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            graph_top: 10,
            config: GraphConfig::default(),
        }
    }
}

/// Format the same fine-grained map as Markdown — same content as
/// `format_text_report` but with real table syntax + headers so it
/// renders nicely in GitHub / VS Code / any MD viewer. Designed to
/// be saved as `report.md` alongside `report.json` so humans and
/// scripts can consume the same analysis without diverging.
///
/// Pre-existing surface; defers to
/// [`format_markdown_report_with_graphs`] with graphs disabled. Kept
/// for legacy callers that don't (yet) want the embedded-graph view.
pub fn format_markdown_report(map: &FineGrainedSymbolMap, top_n: usize) -> String {
    format_markdown_report_with_graphs(
        map,
        top_n,
        &MarkdownGraphOptions {
            enabled: false,
            graph_top: 0,
            config: GraphConfig::default(),
        },
    )
}

/// Same as [`format_markdown_report`] but optionally embeds inline
/// Graphviz `.dot` blocks under each top-N symbol entry (fbuild #463).
/// AI-friendly: a single self-contained `report.md` answers "what
/// pulls in X?" without forcing the agent to fetch a sidecar file.
pub fn format_markdown_report_with_graphs(
    map: &FineGrainedSymbolMap,
    top_n: usize,
    graph_opts: &MarkdownGraphOptions,
) -> String {
    let mut out = String::new();
    use std::fmt::Write as _;
    let flash_count = map
        .symbols
        .iter()
        .filter(|s| s.region == fbuild_core::MemoryRegion::Flash)
        .count();
    let ram_count = map
        .symbols
        .iter()
        .filter(|s| s.region == fbuild_core::MemoryRegion::Ram)
        .count();
    let _ = writeln!(out, "# Symbol analysis: `{}`", map.elf_path);
    let _ = writeln!(out);
    if let Some(ref m) = map.map_path {
        let _ = writeln!(out, "- **Map file**: `{m}`");
    }
    let _ = writeln!(
        out,
        "- **Flash**: {} B across {} sized symbols",
        map.total_flash, flash_count
    );
    let _ = writeln!(
        out,
        "- **RAM**: {} B across {} sized symbols",
        map.total_ram, ram_count
    );
    let _ = writeln!(out);

    fn emit_md_table(
        out: &mut String,
        map: &FineGrainedSymbolMap,
        region: fbuild_core::MemoryRegion,
        top_n: usize,
        title: &str,
    ) {
        use std::fmt::Write as _;
        let mut syms: Vec<_> = map.symbols.iter().filter(|s| s.region == region).collect();
        syms.sort_by(|a, b| b.size.cmp(&a.size));
        let _ = writeln!(
            out,
            "## Top {} {} symbols",
            top_n.min(syms.len()),
            title.to_lowercase()
        );
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "| Bytes | Archive | Object | Section | Source | Referenced by | Symbol |"
        );
        let _ = writeln!(out, "|---:|---|---|---|---|---|---|");
        for s in syms.into_iter().take(top_n) {
            let archive = s.archive.as_deref().unwrap_or("(none)");
            let object = s.object.as_deref().unwrap_or("-");
            let sect = s.output_section.as_deref().unwrap_or("-");
            // Pipe-escape the demangled name so it doesn't break MD
            // table parsing (rare but possible with operator overloads).
            let name = s.demangled.replace('|', "\\|");
            let refs = format_referenced_by(&s.referenced_by, 3);
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} | {} | `{}` |",
                s.size, archive, object, sect, s.source, refs, name
            );
        }
        let _ = writeln!(out);
    }

    emit_md_table(
        &mut out,
        map,
        fbuild_core::MemoryRegion::Flash,
        top_n,
        "FLASH",
    );
    emit_md_table(&mut out, map, fbuild_core::MemoryRegion::Ram, top_n, "RAM");

    if graph_opts.enabled && graph_opts.graph_top > 0 {
        emit_backref_graph_section(&mut out, map, top_n, graph_opts);
    }

    // Per-archive flash roll-up.
    let mut by_archive: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    for s in &map.symbols {
        if s.region != fbuild_core::MemoryRegion::Flash {
            continue;
        }
        let key = s.archive.clone().unwrap_or_else(|| "(unattributed)".into());
        *by_archive.entry(key).or_insert(0) += s.size;
    }
    let mut rows: Vec<(String, u64)> = by_archive.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1));
    let _ = writeln!(out, "## Flash bytes by archive");
    let _ = writeln!(out);
    let _ = writeln!(out, "| Bytes | Archive |");
    let _ = writeln!(out, "|---:|---|");
    for (archive, bytes) in rows.into_iter().take(top_n) {
        let _ = writeln!(out, "| {bytes} | {archive} |");
    }

    out
}

/// Render embedded back-reference graphs for the top symbols
/// directly into the markdown body. Each entry lives under a
/// `<details>` so a long top-10 doesn't crowd the report's eye-line
/// for users who only want the table.
fn emit_backref_graph_section(
    out: &mut String,
    map: &FineGrainedSymbolMap,
    top_n: usize,
    graph_opts: &MarkdownGraphOptions,
) {
    use std::fmt::Write as _;
    let mut syms: Vec<_> = map.symbols.iter().collect();
    syms.sort_by(|a, b| b.size.cmp(&a.size));
    let limit = graph_opts.graph_top.min(top_n).min(syms.len());
    if limit == 0 {
        return;
    }
    let _ = writeln!(
        out,
        "## Top {limit} symbol graphs\n\n\
         For each symbol below: a bidirectional `dot` block (callers on \
         the back-edge side, callees on the forward-edge side), plus a \
         dual-ranked \"Top callees\" sub-table. The forward edges come \
         from per-symbol `references_to` (objdump-derived), so the AI \
         can tell what `ClocklessIdf5` actually calls vs. what its \
         sibling symbols call. See fbuild #463 (backref walker) + \
         #471 (forward edges)."
    );
    let _ = writeln!(out);
    let index = TuIndex::build(map);
    // Per-symbol section: use a bidirectional config so the graph
    // surfaces both axes when forward data is available. If
    // references_to is empty on every row (older fbuild build with no
    // objdump in the chain), the bidirectional walker degenerates
    // gracefully into the same picture the backref-only config would
    // have rendered.
    let mut bidir_cfg = graph_opts.config.clone();
    bidir_cfg.direction = Direction::Bidirectional;
    for (rank, s) in syms.iter().take(limit).enumerate() {
        let archive = s.archive.as_deref().unwrap_or("(none)");
        let object = s.object.as_deref().unwrap_or("-");
        let sect = s.output_section.as_deref().unwrap_or("-");
        let _ = writeln!(
            out,
            "### #{} `{}` — {} B",
            rank + 1,
            s.demangled.replace('|', "\\|"),
            s.size
        );
        let _ = writeln!(out, "- **Archive**: `{archive}`");
        let _ = writeln!(out, "- **Object**: `{object}`");
        let _ = writeln!(out, "- **Section**: `{sect}`");
        let _ = writeln!(out, "- **Referenced by**: {} TUs", s.referenced_by.len());
        let _ = writeln!(
            out,
            "- **References (calls)**: {} symbols",
            s.references_to.len()
        );
        let _ = writeln!(out);

        emit_dual_callers_subtable(out, map, s);
        emit_dual_callees_subtable(out, map, s);

        let graph = BackrefGraph::build_with_index(map, &index, &s.mangled, &bidir_cfg);
        let _ = writeln!(out, "<details>");
        let _ = writeln!(
            out,
            "<summary>Bidirectional graph (callers ← root → callees, Graphviz)</summary>"
        );
        let _ = writeln!(out);
        let _ = writeln!(out, "```dot");
        out.push_str(&graph.to_dot());
        let _ = writeln!(out, "```");
        let _ = writeln!(out);
        let _ = writeln!(out, "</details>");
        let _ = writeln!(out);
    }
}

/// Emit the "Top callers" sub-table for one symbol: per-symbol
/// inverse of [`emit_dual_callees_subtable`]. Two side-by-side
/// rankings (by caller flash bytes, by how many other symbols the
/// caller also calls — proxy for "downstream leverage if this caller
/// were eliminated") plus an `(… and N more)` overflow row.
///
/// Populated from `FineGrainedSymbol::called_by` (objdump-derived,
/// #478). Skipped when the analyzer ran without an objdump (the
/// per-symbol back-reference data isn't available; the existing
/// TU-level `Referenced by` column in the main table still carries
/// the cref-derived view).
fn emit_dual_callers_subtable(
    out: &mut String,
    map: &FineGrainedSymbolMap,
    callee: &fbuild_core::symbol_analysis::FineGrainedSymbol,
) {
    use std::fmt::Write as _;
    if callee.called_by.is_empty() {
        // Don't emit the sub-table header when there's nothing to
        // show — the parent template already prints the TU-level
        // `Referenced by` count, so a blank caller table would just
        // be noise.
        return;
    }
    let (by_size, by_breadth, other) = rank_callers_dual(map, callee, 3);
    let _ = writeln!(out, "#### Top callers (dual ranking)");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "| # | by caller size (B) | calls × | by caller breadth (callees ×) | size (B) |"
    );
    let _ = writeln!(out, "|---:|---|---:|---|---:|");
    for i in 0..3 {
        let size_cell = by_size
            .get(i)
            .map(|c| format!("`{}` — {} B", c.demangled.replace('|', "\\|"), c.size))
            .unwrap_or_else(|| "—".into());
        let size_breadth = by_size
            .get(i)
            .map(|c| c.callees_count.to_string())
            .unwrap_or_else(|| "—".into());
        let breadth_cell = by_breadth
            .get(i)
            .map(|c| {
                format!(
                    "`{}` — calls ×{}",
                    c.demangled.replace('|', "\\|"),
                    c.callees_count
                )
            })
            .unwrap_or_else(|| "—".into());
        let breadth_size = by_breadth
            .get(i)
            .map(|c| c.size.to_string())
            .unwrap_or_else(|| "—".into());
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} |",
            i + 1,
            size_cell,
            size_breadth,
            breadth_cell,
            breadth_size
        );
    }
    if other > 0 {
        let _ = writeln!(
            out,
            "| — | _(… and {other} more callers, see graph below)_ | | | |"
        );
    }
    let _ = writeln!(out);
}

/// Emit the "Top callees" sub-table for one symbol: two side-by-side
/// rankings (by callee flash bytes, by how widely the callee is
/// shared) plus an `(… and N more)` other-bucket row when applicable.
/// This is the data the user asked for in #471 so an AI optimization
/// pass sees a symbol's actual heavy hitters, not just the symbol
/// itself.
fn emit_dual_callees_subtable(
    out: &mut String,
    map: &FineGrainedSymbolMap,
    caller: &fbuild_core::symbol_analysis::FineGrainedSymbol,
) {
    use std::fmt::Write as _;
    if caller.references_to.is_empty() {
        let _ = writeln!(
            out,
            "_No forward-call data for this symbol — either the \
             analyzer wasn't wired with an objdump or the symbol \
             contains no recognised call instructions (data / weak \
             ref / vtable-only dispatch)._"
        );
        let _ = writeln!(out);
        return;
    }
    let (by_size, by_pop, other) = rank_callees_dual(map, caller, 3);
    let _ = writeln!(out, "#### Top callees (dual ranking)");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "| # | by callee size (B) | shared × | by callees \
         shared with (×) | size (B) |"
    );
    let _ = writeln!(out, "|---:|---|---:|---|---:|");
    for i in 0..3 {
        let size_cell = by_size
            .get(i)
            .map(|c| format!("`{}` — {} B", c.demangled.replace('|', "\\|"), c.size))
            .unwrap_or_else(|| "—".into());
        let size_shared = by_size
            .get(i)
            .map(|c| c.callers_count.to_string())
            .unwrap_or_else(|| "—".into());
        let pop_cell = by_pop
            .get(i)
            .map(|c| {
                format!(
                    "`{}` — shared ×{}",
                    c.demangled.replace('|', "\\|"),
                    c.callers_count
                )
            })
            .unwrap_or_else(|| "—".into());
        let pop_size = by_pop
            .get(i)
            .map(|c| c.size.to_string())
            .unwrap_or_else(|| "—".into());
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} |",
            i + 1,
            size_cell,
            size_shared,
            pop_cell,
            pop_size
        );
    }
    if other > 0 {
        let _ = writeln!(
            out,
            "| — | _(… and {other} more callees, see graph below)_ | | | |"
        );
    }
    let _ = writeln!(out);
}

/// Configuration for [`write_sidecar_dot_files`].
#[derive(Debug, Clone)]
pub struct SidecarOptions {
    pub enabled: bool,
    /// Minimum symbol size (bytes) for a sidecar `.dot` file to be
    /// emitted. Default 256 — keeps the output directory to symbols
    /// that meaningfully contribute to flash.
    pub min_bytes: u64,
    /// Walker / rendering configuration shared with the embedded
    /// markdown graphs.
    pub config: GraphConfig,
}

impl Default for SidecarOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            min_bytes: 256,
            config: GraphConfig::default(),
        }
    }
}

/// Write sidecar `.dot` files for every symbol whose size meets
/// `options.min_bytes`. Files live at `<output_dir>/graphs/<rank>_<sanitized>.dot`
/// where `rank` is the symbol's index in the size-descending order
/// (1-based). Returns the number of files written.
///
/// Best-effort — never fails the whole report just because one
/// filesystem write errored; logs a `tracing::warn!` and moves on.
pub fn write_sidecar_dot_files(
    map: &FineGrainedSymbolMap,
    output_dir: &Path,
    options: &SidecarOptions,
) -> Result<usize> {
    if !options.enabled {
        return Ok(0);
    }
    let graphs_dir = output_dir.join("graphs");
    std::fs::create_dir_all(&graphs_dir).map_err(|e| {
        FbuildError::Io(std::io::Error::new(
            e.kind(),
            format!("create {}: {e}", graphs_dir.display()),
        ))
    })?;
    let mut syms: Vec<_> = map.symbols.iter().collect();
    syms.sort_by(|a, b| b.size.cmp(&a.size));
    let index = TuIndex::build(map);
    let mut written = 0usize;
    for (i, s) in syms.iter().enumerate() {
        if s.size < options.min_bytes {
            continue;
        }
        let rank = i + 1;
        let stem = sanitize_filename(&s.demangled);
        let path = graphs_dir.join(format!("{rank:04}_{stem}.dot"));
        let graph = BackrefGraph::build_with_index(map, &index, &s.mangled, &options.config);
        let dot = graph.to_dot();
        if let Err(e) = std::fs::write(&path, dot) {
            tracing::warn!(
                "sidecar graph {}: write failed ({e}); skipping",
                path.display()
            );
            continue;
        }
        written += 1;
    }
    Ok(written)
}

/// Format up to `top_k` `referenced_by` entries for a Markdown table
/// cell. Each referencer is rendered as `archive(object)` (or just
/// `object` for bare TUs with no archive) and joined with `, `. When
/// the list exceeds `top_k`, append ` (… and N more)`. Returns `-`
/// for an empty list so the column stays scannable.
///
/// `top_k = 3` is the column-friendly default — the issue proposes
/// K=5 as a follow-up-table value, but five `lib.a(obj.o)` strings
/// per row makes the GitHub-rendered table awkward. Three keeps the
/// signal-to-width ratio readable while still surfacing the most
/// common "libc internal wrapper escapes to an ESP-IDF/mbedTLS TU"
/// pattern documented in #459.
fn format_referenced_by(
    refs: &[fbuild_core::symbol_analysis::SymbolReference],
    top_k: usize,
) -> String {
    if refs.is_empty() {
        return "-".to_string();
    }
    let mut parts: Vec<String> = refs
        .iter()
        .take(top_k)
        .map(|r| match &r.archive {
            Some(a) => format!("{a}({})", r.object),
            None => r.object.clone(),
        })
        .collect();
    if refs.len() > top_k {
        parts.push(format!("(… and {} more)", refs.len() - top_k));
    }
    // Pipe-escape so the joined string doesn't break MD table cells.
    parts.join(", ").replace('|', "\\|")
}
