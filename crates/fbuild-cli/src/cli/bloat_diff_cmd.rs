//! `fbuild bloat-diff <a> <b>` — per-symbol delta between two bloat
//! reports.
//!
//! Subsumes the ad-hoc `diff.py` script the FastLED #2773 audit
//! carried. Each input may be a project directory, a `report.json`,
//! or an ELF — same resolution as `fbuild bloat` (#440).
//!
//! Symbol identity is `(demangled, archive, object)` so two symbols
//! with the same fully-qualified name in different archives are
//! treated as distinct. This matches the existing FastLED `diff.py`
//! and is what the audit report depends on.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use fbuild_core::symbol_analysis::{FineGrainedSymbol, FineGrainedSymbolMap};
use fbuild_core::{FbuildError, MemoryRegion, Result};
use serde::{Deserialize, Serialize};

use super::bloat_cmd::{load_or_analyze, InputAnalysis};

/// One row in the per-symbol delta. Encoded as the `kind` field
/// (`added` / `removed` / `grew` / `shrunk`) so JSON consumers can
/// filter without re-running the diff.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolDelta {
    pub kind: String,
    pub demangled: String,
    pub archive: Option<String>,
    pub object: Option<String>,
    pub region: MemoryRegion,
    /// Byte size in A. `None` for added symbols.
    pub size_a: Option<u64>,
    /// Byte size in B. `None` for removed symbols.
    pub size_b: Option<u64>,
    /// `size_b - size_a` cast to i64. Positive = grew / added,
    /// negative = shrunk / removed.
    pub delta: i64,
}

/// Per-archive rollup of net change.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveRollup {
    pub archive: String,
    pub net_flash: i64,
    pub net_ram: i64,
    pub added: usize,
    pub removed: usize,
    pub grew: usize,
    pub shrunk: usize,
}

/// Top-level diff report shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffReport {
    /// Inputs as the user passed them — surfaced in the MD header.
    pub input_a: String,
    pub input_b: String,
    pub net_flash: i64,
    pub net_ram: i64,
    pub deltas: Vec<SymbolDelta>,
    pub by_archive: Vec<ArchiveRollup>,
}

/// CLI entry point.
#[allow(clippy::too_many_arguments)]
pub fn run_bloat_diff(
    a: String,
    b: String,
    map_a: Option<String>,
    map_b: Option<String>,
    nm: Option<String>,
    cppfilt: Option<String>,
    build_info: Option<String>,
    json_out: Option<String>,
    output_dir: Option<String>,
    top: usize,
    region: Option<String>,
) -> Result<()> {
    let a_path = PathBuf::from(&a);
    let b_path = PathBuf::from(&b);
    if !a_path.exists() {
        return Err(FbuildError::BuildFailed(format!(
            "input A not found: {}",
            a_path.display()
        )));
    }
    if !b_path.exists() {
        return Err(FbuildError::BuildFailed(format!(
            "input B not found: {}",
            b_path.display()
        )));
    }

    let region = parse_region(region.as_deref())?;

    let analysis_a = load_or_analyze(
        &a_path,
        map_a.as_deref(),
        nm.as_deref(),
        cppfilt.as_deref(),
        build_info.as_deref(),
    )?;
    let analysis_b = load_or_analyze(
        &b_path,
        map_b.as_deref(),
        nm.as_deref(),
        cppfilt.as_deref(),
        build_info.as_deref(),
    )?;

    let report = compute_diff(&a, &b, &analysis_a.report, &analysis_b.report, Some(region));

    if let Some(json_path) = json_out.as_deref() {
        write_json(&report, json_path)?;
        println!("Wrote diff to {json_path}");
        return Ok(());
    }

    let dir = match output_dir {
        Some(s) => PathBuf::from(s),
        None => default_diff_output_dir(&a_path, &b_path, analysis_a.into(), analysis_b.into()),
    };
    write_dual_report(&report, &dir, top, region)?;
    print_diff_summary(&report, &dir);
    Ok(())
}

fn parse_region(region: Option<&str>) -> Result<MemoryRegion> {
    match region.unwrap_or("flash") {
        "flash" => Ok(MemoryRegion::Flash),
        "ram" => Ok(MemoryRegion::Ram),
        other => Err(FbuildError::BuildFailed(format!(
            "unknown --region {other:?}: expected `flash` or `ram`"
        ))),
    }
}

/// Coerce an [`InputAnalysis`] into the minimal `(elf_path,
/// project_env)` info `default_diff_output_dir` needs without
/// exposing private types across modules.
impl From<InputAnalysis> for AnalysisHandle {
    fn from(a: InputAnalysis) -> Self {
        AnalysisHandle {
            elf_path: a.elf_path,
            project_env: a.project.map(|p| (p.project_dir, p.env_name)),
        }
    }
}

struct AnalysisHandle {
    elf_path: Option<PathBuf>,
    /// (project_dir, env_name) when the input had a build_info.json
    /// alongside it.
    project_env: Option<(PathBuf, String)>,
}

/// Default output dir for a bloat-diff invocation: `<cwd>/bloat-diff/
/// <a-stem>__vs__<b-stem>/`. When both inputs came from the same
/// project, prefer `<project>/.fbuild/build/<env>/bloat-diff/
/// <a-stem>__vs__<b-stem>/` so the diff lands next to the build
/// artifacts (mirrors `fbuild bloat`).
fn default_diff_output_dir(
    a_path: &Path,
    b_path: &Path,
    a: AnalysisHandle,
    b: AnalysisHandle,
) -> PathBuf {
    let stem = format!(
        "{}__vs__{}",
        path_stem(a_path).unwrap_or("a"),
        path_stem(b_path).unwrap_or("b"),
    );
    // Prefer the shared project layout when both sides agree.
    if let (Some((pa, ea)), Some((pb, eb))) = (a.project_env.as_ref(), b.project_env.as_ref()) {
        if pa == pb && ea == eb {
            return pa
                .join(".fbuild")
                .join("build")
                .join(ea)
                .join("bloat-diff")
                .join(stem);
        }
    }
    // Otherwise: side A's project, or A's ELF parent, or just cwd.
    if let Some((p, e)) = a.project_env {
        return p
            .join(".fbuild")
            .join("build")
            .join(e)
            .join("bloat-diff")
            .join(stem);
    }
    if let Some(parent) = a.elf_path.as_ref().and_then(|p| p.parent()) {
        return parent.join("bloat-diff").join(stem);
    }
    PathBuf::from("bloat-diff").join(stem)
}

fn path_stem(p: &Path) -> Option<&str> {
    p.file_stem().and_then(|s| s.to_str())
}

/// Build the diff report by matching symbols on `(demangled,
/// archive, object)`. When `region_filter` is `Some`, only that
/// region's symbols are included in `deltas` — but the totals on
/// `net_flash` / `net_ram` are always computed from both regions so
/// the summary tells the truth.
pub fn compute_diff(
    input_a: &str,
    input_b: &str,
    a: &FineGrainedSymbolMap,
    b: &FineGrainedSymbolMap,
    region_filter: Option<MemoryRegion>,
) -> DiffReport {
    let mut index_a: BTreeMap<SymbolKey, &FineGrainedSymbol> = BTreeMap::new();
    for sym in &a.symbols {
        index_a.insert(key(sym), sym);
    }
    let mut index_b: BTreeMap<SymbolKey, &FineGrainedSymbol> = BTreeMap::new();
    for sym in &b.symbols {
        index_b.insert(key(sym), sym);
    }

    let mut deltas: Vec<SymbolDelta> = Vec::new();
    let mut net_flash: i64 = 0;
    let mut net_ram: i64 = 0;

    // Added or changed.
    for (k, sb) in &index_b {
        match index_a.get(k) {
            None => {
                accumulate_net(sb.region, sb.size as i64, &mut net_flash, &mut net_ram);
                push_if_region(
                    region_filter,
                    sb.region,
                    &mut deltas,
                    SymbolDelta {
                        kind: "added".to_string(),
                        demangled: sb.demangled.clone(),
                        archive: sb.archive.clone(),
                        object: sb.object.clone(),
                        region: sb.region,
                        size_a: None,
                        size_b: Some(sb.size),
                        delta: sb.size as i64,
                    },
                );
            }
            Some(sa) => {
                let delta = sb.size as i64 - sa.size as i64;
                if delta == 0 {
                    continue;
                }
                accumulate_net(sb.region, delta, &mut net_flash, &mut net_ram);
                let kind = if delta > 0 { "grew" } else { "shrunk" };
                push_if_region(
                    region_filter,
                    sb.region,
                    &mut deltas,
                    SymbolDelta {
                        kind: kind.to_string(),
                        demangled: sb.demangled.clone(),
                        archive: sb.archive.clone(),
                        object: sb.object.clone(),
                        region: sb.region,
                        size_a: Some(sa.size),
                        size_b: Some(sb.size),
                        delta,
                    },
                );
            }
        }
    }
    // Removed.
    for (k, sa) in &index_a {
        if !index_b.contains_key(k) {
            accumulate_net(sa.region, -(sa.size as i64), &mut net_flash, &mut net_ram);
            push_if_region(
                region_filter,
                sa.region,
                &mut deltas,
                SymbolDelta {
                    kind: "removed".to_string(),
                    demangled: sa.demangled.clone(),
                    archive: sa.archive.clone(),
                    object: sa.object.clone(),
                    region: sa.region,
                    size_a: Some(sa.size),
                    size_b: None,
                    delta: -(sa.size as i64),
                },
            );
        }
    }

    // Sort by absolute delta, descending — biggest movers first.
    deltas.sort_by(|x, y| y.delta.abs().cmp(&x.delta.abs()));

    let by_archive = rollup_by_archive(&deltas);

    DiffReport {
        input_a: input_a.to_string(),
        input_b: input_b.to_string(),
        net_flash,
        net_ram,
        deltas,
        by_archive,
    }
}

fn push_if_region(
    filter: Option<MemoryRegion>,
    sym_region: MemoryRegion,
    out: &mut Vec<SymbolDelta>,
    delta: SymbolDelta,
) {
    match filter {
        Some(r) if r != sym_region => {}
        _ => out.push(delta),
    }
}

fn accumulate_net(region: MemoryRegion, delta: i64, net_flash: &mut i64, net_ram: &mut i64) {
    match region {
        MemoryRegion::Flash => *net_flash += delta,
        MemoryRegion::Ram => *net_ram += delta,
    }
}

type SymbolKey = (String, Option<String>, Option<String>);

fn key(sym: &FineGrainedSymbol) -> SymbolKey {
    (
        sym.demangled.clone(),
        sym.archive.clone(),
        sym.object.clone(),
    )
}

fn rollup_by_archive(deltas: &[SymbolDelta]) -> Vec<ArchiveRollup> {
    let mut by: BTreeMap<String, ArchiveRollup> = BTreeMap::new();
    for d in deltas {
        let key = d.archive.clone().unwrap_or_else(|| "<no-archive>".into());
        let row = by.entry(key.clone()).or_insert(ArchiveRollup {
            archive: key,
            net_flash: 0,
            net_ram: 0,
            added: 0,
            removed: 0,
            grew: 0,
            shrunk: 0,
        });
        match d.region {
            MemoryRegion::Flash => row.net_flash += d.delta,
            MemoryRegion::Ram => row.net_ram += d.delta,
        }
        match d.kind.as_str() {
            "added" => row.added += 1,
            "removed" => row.removed += 1,
            "grew" => row.grew += 1,
            "shrunk" => row.shrunk += 1,
            _ => {}
        }
    }
    let mut out: Vec<ArchiveRollup> = by.into_values().collect();
    out.sort_by(|x, y| {
        (y.net_flash.abs() + y.net_ram.abs()).cmp(&(x.net_flash.abs() + x.net_ram.abs()))
    });
    out
}

fn write_json(report: &DiffReport, json_path: &str) -> Result<()> {
    let json = serde_json::to_string_pretty(report)
        .map_err(|e| FbuildError::Other(format!("json serialize: {e}")))?;
    std::fs::write(json_path, json).map_err(|e| {
        FbuildError::Io(std::io::Error::new(
            e.kind(),
            format!("write {json_path}: {e}"),
        ))
    })?;
    Ok(())
}

fn write_dual_report(
    report: &DiffReport,
    dir: &Path,
    top: usize,
    region: MemoryRegion,
) -> Result<()> {
    std::fs::create_dir_all(dir).map_err(|e| {
        FbuildError::Io(std::io::Error::new(
            e.kind(),
            format!("create {}: {e}", dir.display()),
        ))
    })?;
    let json_target = dir.join("delta-report.json");
    let md_target = dir.join("delta-report.md");
    write_json(report, &json_target.to_string_lossy())?;
    let md = format_diff_markdown(report, top, region);
    std::fs::write(&md_target, md).map_err(|e| {
        FbuildError::Io(std::io::Error::new(
            e.kind(),
            format!("write {}: {e}", md_target.display()),
        ))
    })?;
    Ok(())
}

fn print_diff_summary(report: &DiffReport, dir: &Path) {
    println!("Bloat diff:");
    println!("  A: {}", report.input_a);
    println!("  B: {}", report.input_b);
    println!(
        "  Net Flash: {:+} B  Net RAM: {:+} B",
        report.net_flash, report.net_ram
    );
    println!();
    println!("Written to:");
    println!("  {}", dir.join("delta-report.json").display());
    println!("  {}", dir.join("delta-report.md").display());
}

/// Render a GitHub-friendly Markdown table for the diff. Focused on
/// `region` so the report reads cleanly when looking at one memory
/// kind at a time (which is what the audit workflow actually does).
pub fn format_diff_markdown(report: &DiffReport, top: usize, region: MemoryRegion) -> String {
    let mut out = String::new();
    out.push_str("# Bloat diff\n\n");
    out.push_str(&format!("- **A:** `{}`\n", report.input_a));
    out.push_str(&format!("- **B:** `{}`\n", report.input_b));
    out.push_str(&format!(
        "- **Net Flash:** {:+} B\n- **Net RAM:** {:+} B\n\n",
        report.net_flash, report.net_ram
    ));

    let label = match region {
        MemoryRegion::Flash => "flash",
        MemoryRegion::Ram => "ram",
    };
    out.push_str(&format!("## Top {top} {label} movers\n\n"));
    out.push_str("| Delta | Kind | A → B | Archive | Symbol |\n");
    out.push_str("|------:|:-----|:------|:--------|:-------|\n");
    let mut shown = 0usize;
    for d in &report.deltas {
        if d.region != region {
            continue;
        }
        if shown >= top {
            break;
        }
        let a = d
            .size_a
            .map(|n| n.to_string())
            .unwrap_or_else(|| "—".into());
        let b = d
            .size_b
            .map(|n| n.to_string())
            .unwrap_or_else(|| "—".into());
        let archive = d.archive.as_deref().unwrap_or("—");
        out.push_str(&format!(
            "| {:+} | {} | {} → {} | {} | `{}` |\n",
            d.delta, d.kind, a, b, archive, d.demangled
        ));
        shown += 1;
    }

    out.push_str("\n## Per-archive rollup\n\n");
    out.push_str("| Archive | Net Flash | Net RAM | Added | Removed | Grew | Shrunk |\n");
    out.push_str("|---------|----------:|--------:|------:|--------:|-----:|-------:|\n");
    for r in &report.by_archive {
        out.push_str(&format!(
            "| `{}` | {:+} | {:+} | {} | {} | {} | {} |\n",
            r.archive, r.net_flash, r.net_ram, r.added, r.removed, r.grew, r.shrunk
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sym(
        name: &str,
        archive: &str,
        object: &str,
        size: u64,
        region: MemoryRegion,
    ) -> FineGrainedSymbol {
        FineGrainedSymbol {
            mangled: name.to_string(),
            demangled: name.to_string(),
            address: 0,
            size,
            sym_type: 'T',
            region,
            archive: Some(archive.to_string()),
            object: Some(object.to_string()),
            output_section: Some(".text".to_string()),
            source: "nm".to_string(),
        }
    }

    fn map_with(symbols: Vec<FineGrainedSymbol>) -> FineGrainedSymbolMap {
        let total_flash = symbols
            .iter()
            .filter(|s| s.region == MemoryRegion::Flash)
            .map(|s| s.size)
            .sum();
        let total_ram = symbols
            .iter()
            .filter(|s| s.region == MemoryRegion::Ram)
            .map(|s| s.size)
            .sum();
        FineGrainedSymbolMap {
            elf_path: "/tmp/firmware.elf".to_string(),
            map_path: None,
            total_flash,
            total_ram,
            symbols,
            sections: vec![],
        }
    }

    #[test]
    fn diff_categorises_added_removed_grew_shrunk() {
        let a = map_with(vec![
            sym("foo", "libA.a", "foo.o", 100, MemoryRegion::Flash),
            sym("bar", "libA.a", "bar.o", 50, MemoryRegion::Flash),
            sym("zap", "libB.a", "zap.o", 30, MemoryRegion::Ram),
        ]);
        let b = map_with(vec![
            // foo grew by 50.
            sym("foo", "libA.a", "foo.o", 150, MemoryRegion::Flash),
            // bar shrunk by 10.
            sym("bar", "libA.a", "bar.o", 40, MemoryRegion::Flash),
            // zap removed.
            // baz added (new).
            sym("baz", "libC.a", "baz.o", 25, MemoryRegion::Flash),
        ]);
        let report = compute_diff("a", "b", &a, &b, None);
        let kinds: std::collections::BTreeSet<_> =
            report.deltas.iter().map(|d| d.kind.clone()).collect();
        assert!(kinds.contains("added"));
        assert!(kinds.contains("removed"));
        assert!(kinds.contains("grew"));
        assert!(kinds.contains("shrunk"));
    }

    #[test]
    fn diff_net_totals_are_correct() {
        let a = map_with(vec![sym(
            "foo",
            "libA.a",
            "foo.o",
            100,
            MemoryRegion::Flash,
        )]);
        let b = map_with(vec![sym(
            "foo",
            "libA.a",
            "foo.o",
            150,
            MemoryRegion::Flash,
        )]);
        let report = compute_diff("a", "b", &a, &b, None);
        assert_eq!(report.net_flash, 50);
        assert_eq!(report.net_ram, 0);
    }

    #[test]
    fn diff_filters_by_region() {
        let a = map_with(vec![
            sym("flash_sym", "libA.a", "f.o", 100, MemoryRegion::Flash),
            sym("ram_sym", "libA.a", "r.o", 50, MemoryRegion::Ram),
        ]);
        let b = map_with(vec![
            sym("flash_sym", "libA.a", "f.o", 150, MemoryRegion::Flash),
            sym("ram_sym", "libA.a", "r.o", 80, MemoryRegion::Ram),
        ]);
        let report = compute_diff("a", "b", &a, &b, Some(MemoryRegion::Ram));
        assert!(report.deltas.iter().all(|d| d.region == MemoryRegion::Ram));
        // Net totals are still both regions (the summary tells the truth).
        assert_eq!(report.net_flash, 50);
        assert_eq!(report.net_ram, 30);
    }

    #[test]
    fn diff_distinguishes_same_name_in_different_archives() {
        // Two symbols with the same demangled name in different archives
        // are treated as distinct (matches FastLED diff.py).
        let a = map_with(vec![
            sym("common", "libA.a", "x.o", 100, MemoryRegion::Flash),
            sym("common", "libB.a", "y.o", 200, MemoryRegion::Flash),
        ]);
        let b = map_with(vec![
            sym("common", "libA.a", "x.o", 150, MemoryRegion::Flash),
            // libB's `common` removed.
        ]);
        let report = compute_diff("a", "b", &a, &b, None);
        let grew = report.deltas.iter().filter(|d| d.kind == "grew").count();
        let removed = report.deltas.iter().filter(|d| d.kind == "removed").count();
        assert_eq!(grew, 1);
        assert_eq!(removed, 1);
    }

    #[test]
    fn diff_rollup_by_archive_aggregates_net_change() {
        let a = map_with(vec![
            sym("foo", "libA.a", "foo.o", 100, MemoryRegion::Flash),
            sym("bar", "libA.a", "bar.o", 50, MemoryRegion::Flash),
            sym("zap", "libB.a", "zap.o", 30, MemoryRegion::Flash),
        ]);
        let b = map_with(vec![
            sym("foo", "libA.a", "foo.o", 200, MemoryRegion::Flash), // +100
            sym("bar", "libA.a", "bar.o", 50, MemoryRegion::Flash),  // 0
                                                                     // zap removed → -30 libB
        ]);
        let report = compute_diff("a", "b", &a, &b, None);
        let lib_a = report
            .by_archive
            .iter()
            .find(|r| r.archive == "libA.a")
            .expect("libA.a rollup");
        let lib_b = report
            .by_archive
            .iter()
            .find(|r| r.archive == "libB.a")
            .expect("libB.a rollup");
        assert_eq!(lib_a.net_flash, 100);
        assert_eq!(lib_a.grew, 1);
        assert_eq!(lib_b.net_flash, -30);
        assert_eq!(lib_b.removed, 1);
    }

    #[test]
    fn parse_region_defaults_to_flash() {
        assert_eq!(parse_region(None).unwrap(), MemoryRegion::Flash);
        assert_eq!(parse_region(Some("flash")).unwrap(), MemoryRegion::Flash);
        assert_eq!(parse_region(Some("ram")).unwrap(), MemoryRegion::Ram);
        assert!(parse_region(Some("rom")).is_err());
    }

    #[test]
    fn format_diff_markdown_emits_expected_sections() {
        let a = map_with(vec![sym("foo", "libA.a", "f.o", 100, MemoryRegion::Flash)]);
        let b = map_with(vec![sym("foo", "libA.a", "f.o", 200, MemoryRegion::Flash)]);
        let report = compute_diff("a-path", "b-path", &a, &b, None);
        let md = format_diff_markdown(&report, 10, MemoryRegion::Flash);
        assert!(md.contains("# Bloat diff"));
        assert!(md.contains("a-path"));
        assert!(md.contains("b-path"));
        assert!(md.contains("Top 10 flash movers"));
        assert!(md.contains("Per-archive rollup"));
        assert!(md.contains("foo"));
        assert!(md.contains("libA.a"));
    }
}
