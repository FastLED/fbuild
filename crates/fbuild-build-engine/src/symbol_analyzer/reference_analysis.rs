//! Address-qualified reference attribution; names alone cannot identify fragments.
use fbuild_core::symbol_analysis::*;
use fbuild_core::{FbuildError, MemoryRegion, Result};
use object::{Object, ObjectSection, ObjectSymbol};
use std::collections::BTreeSet;
use std::path::Path;

fn normalized(address: u64, arm: bool) -> u64 {
    if arm { address & !1 } else { address }
}

fn identity(map: &FineGrainedSymbolMap, name: &str, address: u64, arm: bool) -> SymbolIdentity {
    map.symbols
        .iter()
        .find(|s| s.source == "nm" && s.mangled == name && normalized(s.address, arm) == address)
        .map(SymbolIdentity::from)
        .unwrap_or_else(|| SymbolIdentity {
            name: name.into(),
            address,
            source: "external".into(),
        })
}

pub(super) fn attribute_disassembly(
    map: &mut FineGrainedSymbolMap,
    text: &str,
    arm: bool,
) -> usize {
    let index: std::collections::BTreeMap<_, _> = map
        .symbols
        .iter()
        .filter(|s| s.source == "nm")
        .map(|s| {
            (
                (s.mangled.clone(), normalized(s.address, arm)),
                SymbolIdentity::from(s),
            )
        })
        .collect();
    let resolve = |name: &str, address: u64| {
        index
            .get(&(name.to_string(), address))
            .cloned()
            .unwrap_or_else(|| SymbolIdentity {
                name: name.into(),
                address,
                source: "external".into(),
            })
    };
    let mut current: Option<SymbolIdentity> = None;
    let mut seen = BTreeSet::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Disassembly of section") {
            current = None;
            continue;
        }
        if let Some(header) = trimmed.strip_suffix(":").and_then(|h| h.split_once(" <")) {
            if let (Ok(address), Some(name)) = (
                u64::from_str_radix(header.0.trim_start_matches("0x"), 16),
                header.1.strip_suffix('>'),
            ) {
                current = Some(resolve(name, normalized(address, arm)));
                continue;
            }
        }
        let Some(source) = current.as_ref() else {
            continue;
        };
        if source.source == "external" {
            continue;
        }
        let Some((_, instruction)) = trimmed.split_once(':') else {
            continue;
        };
        let Some(close) = instruction.rfind('>') else {
            continue;
        };
        let Some(open) = instruction[..close].rfind('<') else {
            continue;
        };
        let name = &instruction[open + 1..close];
        if name.contains("+0x")
            || name.contains("-0x")
            || name.ends_with("@plt")
            || name.starts_with('$')
        {
            continue;
        }
        let Some(address_text) = instruction[..open].split_whitespace().last() else {
            continue;
        };
        let Ok(address) = u64::from_str_radix(address_text.trim_start_matches("0x"), 16) else {
            continue;
        };
        let target = resolve(name, normalized(address, arm));
        if source == &target || !seen.insert((source.clone(), target.clone())) {
            continue;
        }
        map.reference_analysis.edges.push(ReferenceEdge {
            source: source.clone(),
            target,
            kind: ReferenceKind::Disassembly,
            offset: None,
        });
    }
    populate_legacy_lists(map);
    seen.len()
}

fn populate_legacy_lists(map: &mut FineGrainedSymbolMap) {
    // Legacy name lists retain instruction references only. Static pointer
    // owners remain separately typed so they are never advertised as callers.
    let mut forward = std::collections::BTreeMap::<SymbolIdentity, BTreeSet<String>>::new();
    let mut backward = std::collections::BTreeMap::<SymbolIdentity, BTreeSet<String>>::new();
    for edge in &map.reference_analysis.edges {
        if edge.kind != ReferenceKind::Disassembly {
            continue;
        }
        forward
            .entry(edge.source.clone())
            .or_default()
            .insert(edge.target.name.clone());
        backward
            .entry(edge.target.clone())
            .or_default()
            .insert(edge.source.name.clone());
    }
    for symbol in &mut map.symbols {
        let id = SymbolIdentity::from(&*symbol);
        symbol.references_to = forward
            .remove(&id)
            .unwrap_or_default()
            .into_iter()
            .collect();
        symbol.called_by = backward
            .remove(&id)
            .unwrap_or_default()
            .into_iter()
            .collect();
    }
}

pub(super) fn probe_elf(map: &mut FineGrainedSymbolMap, path: &Path) -> Result<bool> {
    let bytes = std::fs::read(path).map_err(FbuildError::Io)?;
    let file = object::File::parse(bytes.as_slice())
        .map_err(|e| FbuildError::BuildFailed(format!("reference ELF probe: {e}")))?;
    if !matches!(
        file.kind(),
        object::ObjectKind::Executable | object::ObjectKind::Dynamic
    ) {
        return Err(FbuildError::BuildFailed(
            "Reference roots require a final executable/shared ELF, not a relocatable object."
                .into(),
        ));
    }
    let arm = file.architecture() == object::Architecture::Arm;
    // nm's weak-object V/v does not encode its storage region. Use the
    // allocated ELF section, not a Flash guess or a mangled-name heuristic.
    let mut weak = std::collections::BTreeMap::new();
    for symbol in file.symbols() {
        let (Ok(name), Some(section_index)) = (symbol.name(), symbol.section_index()) else {
            continue;
        };
        let section = file
            .section_by_index(section_index)
            .map_err(|e| FbuildError::BuildFailed(e.to_string()))?;
        if !matches!(section.flags(), object::SectionFlags::Elf {sh_flags} if sh_flags & u64::from(object::elf::SHF_ALLOC) != 0)
        {
            continue;
        }
        let region = if section.kind() == object::SectionKind::UninitializedData
            || matches!(section.flags(), object::SectionFlags::Elf {sh_flags} if sh_flags & u64::from(object::elf::SHF_WRITE) != 0)
        {
            MemoryRegion::Ram
        } else {
            MemoryRegion::Flash
        };
        weak.insert(
            (name.to_string(), symbol.address()),
            (region, section.name().unwrap_or("").to_string()),
        );
    }
    map.symbols.retain_mut(|symbol| {
        if !matches!(symbol.sym_type, 'V' | 'v') {
            return true;
        }
        let Some((region, section)) = weak.get(&(symbol.mangled.clone(), symbol.address)) else {
            return false;
        };
        symbol.region = *region;
        symbol.output_section = Some(section.clone());
        true
    });
    map.total_flash = map
        .symbols
        .iter()
        .filter(|s| s.region == MemoryRegion::Flash)
        .map(|s| s.size)
        .sum();
    map.total_ram = map
        .symbols
        .iter()
        .filter(|s| s.region == MemoryRegion::Ram)
        .map(|s| s.size)
        .sum();
    let entry = normalized(file.entry(), arm);
    for symbol in file.symbols() {
        if symbol.is_definition()
            && symbol.kind() == object::SymbolKind::Text
            && normalized(symbol.address(), arm) == entry
        {
            if let Ok(name) = symbol.name() {
                map.reference_analysis.roots.push(RetentionRoot {
                    symbol: identity(map, name, entry, arm),
                    kind: "entry_point".into(),
                });
            }
        }
    }
    Ok(arm)
}

pub(super) fn static_edges(map: &mut FineGrainedSymbolMap, path: &Path, arm: bool) {
    match super::elf_references::read_vtable_references(path) {
        Ok(edges) => {
            map.reference_analysis.static_data.status = AnalysisStatus::Analyzed;
            for edge in edges {
                let source = identity(map, &edge.source_name, edge.source_address, arm);
                let target = identity(map, &edge.target_name, edge.target_address, arm);
                map.reference_analysis.edges.push(ReferenceEdge {
                    source,
                    target,
                    kind: ReferenceKind::StaticData,
                    offset: Some(edge.offset),
                });
            }
        }
        Err(e) => {
            map.reference_analysis.static_data.status = AnalysisStatus::Error;
            map.reference_analysis.static_data.reason = Some(e.to_string());
        }
    }
}

pub(super) fn finalize(map: &mut FineGrainedSymbolMap) {
    // Map-derived pools carry a compiler-provided owning function name.
    // Keep ownership separate from a machine instruction or pointer edge.
    let mut owners = std::collections::BTreeMap::<_, Vec<&FineGrainedSymbol>>::new();
    for owner in &map.symbols {
        if owner.source != "nm" || !matches!(owner.sym_type, 'T' | 't' | 'W' | 'w') {
            continue;
        }
        if let Some(object) = owner.object.as_deref() {
            owners
                .entry((owner.mangled.as_str(), owner.archive.as_deref(), object))
                .or_default()
                .push(owner);
        }
    }
    for fragment in &map.symbols {
        if fragment.source != "map-derived" {
            continue;
        }
        let Some(object) = fragment.object.as_deref() else {
            continue;
        };
        let Some(candidates) = owners.get(&(
            fragment.mangled.as_str(),
            fragment.archive.as_deref(),
            object,
        )) else {
            continue;
        };
        if let [owner] = candidates.as_slice() {
            map.reference_analysis.edges.push(ReferenceEdge {
                source: (*owner).into(),
                target: fragment.into(),
                kind: ReferenceKind::FragmentOwner,
                offset: None,
            });
        }
    }
    let incoming: BTreeSet<_> = map
        .reference_analysis
        .edges
        .iter()
        .map(|e| e.target.clone())
        .collect();
    let roots: BTreeSet<_> = map
        .reference_analysis
        .roots
        .iter()
        .map(|r| r.symbol.clone())
        .collect();
    map.reference_analysis.unexplained = map
        .symbols
        .iter()
        .filter(|s| {
            s.size > 0
                && s.referenced_by.is_empty()
                && !incoming.contains(&SymbolIdentity::from(*s))
                && !roots.contains(&SymbolIdentity::from(*s))
        })
        .map(SymbolIdentity::from)
        .collect();
    let unresolved: BTreeSet<_> = map
        .reference_analysis
        .edges
        .iter()
        .flat_map(|e| [&e.source, &e.target])
        .filter(|id| id.source == "external")
        .map(|id| (id.name.clone(), id.address))
        .collect();
    map.reference_analysis.unresolved = unresolved
        .into_iter()
        .map(|(name, address)| UnresolvedReference { name, address })
        .collect();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disassembly_edges_do_not_leak_into_same_name_rodata_fragments() {
        let mut map = build_fine_grained_map(
            "x".into(),
            None,
            vec![
                (0x100, 16, 'T', "caller".into()),
                (0x200, 16, 'T', "target".into()),
                (0x300, 8, 'r', "target".into()),
            ],
            vec!["caller".into(), "target".into(), "target".into()],
            vec![],
        );
        map.symbols[2].source = "map-derived".into();
        assert_eq!(
            attribute_disassembly(
                &mut map,
                "00000100 <caller>:\n 100: call 200 <target>\n",
                false
            ),
            1
        );
        assert_eq!(map.symbols[1].called_by, vec!["caller"]);
        assert!(map.symbols[2].called_by.is_empty());
        assert_eq!(map.reference_analysis.edges[0].target.address, 0x200);
    }
    #[test]
    fn missing_local_or_rom_symbols_remain_addressed_external_targets() {
        let mut map = build_fine_grained_map(
            "x".into(),
            None,
            vec![(0x100, 16, 'T', "caller".into())],
            vec!["caller".into()],
            vec![],
        );
        assert_eq!(
            attribute_disassembly(
                &mut map,
                "00000100 <caller>:\n 100: call 400 <rom_func>\n",
                false
            ),
            1
        );
        let edge = &map.reference_analysis.edges[0];
        assert_eq!(edge.target.source, "external");
        assert_eq!(edge.target.address, 0x400);
    }
    #[test]
    fn same_name_local_fragment_owners_require_matching_object_provenance() {
        let mut map = build_fine_grained_map(
            "x".into(),
            None,
            vec![
                (0x100, 16, 'T', "local_fn".into()),
                (0x200, 16, 'T', "local_fn".into()),
                (0x300, 8, 'r', "local_fn".into()),
                (0x400, 8, 'r', "local_fn".into()),
            ],
            vec!["local_fn".into(); 4],
            vec![],
        );
        for (i, symbol) in map.symbols.iter_mut().enumerate() {
            symbol.object = Some(if i % 2 == 0 { "first.o" } else { "second.o" }.into());
            if i >= 2 {
                symbol.source = "map-derived".into();
            }
        }
        finalize(&mut map);
        assert_eq!(map.reference_analysis.edges.len(), 2);
        assert!(
            map.reference_analysis
                .edges
                .iter()
                .all(|e| (e.source.address == 0x100 && e.target.address == 0x300)
                    || (e.source.address == 0x200 && e.target.address == 0x400))
        );
    }
}
