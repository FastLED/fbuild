//! Verified static function pointers in allocated Itanium C++ vtables.
//! This deliberately does not scan arbitrary data for address-shaped integers.
use std::collections::BTreeMap;
use std::path::Path;

use fbuild_core::{FbuildError, Result};
use object::{Object, ObjectSection, ObjectSymbol};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticDataReference {
    pub source_name: String,
    pub source_address: u64,
    pub target_name: String,
    pub target_address: u64,
    pub offset: u64,
}

pub fn read_vtable_references(elf_path: &Path) -> Result<Vec<StaticDataReference>> {
    let bytes = std::fs::read(elf_path).map_err(|e| {
        FbuildError::BuildFailed(format!("could not read ELF {}: {e}", elf_path.display()))
    })?;
    references_from_bytes(&bytes)
}

fn references_from_bytes(bytes: &[u8]) -> Result<Vec<StaticDataReference>> {
    let file = object::File::parse(bytes)
        .map_err(|e| FbuildError::BuildFailed(format!("ELF static reference parse failed: {e}")))?;
    if file.format() != object::BinaryFormat::Elf {
        return Err(FbuildError::BuildFailed(
            "static references require ELF".into(),
        ));
    }
    if !matches!(
        file.kind(),
        object::ObjectKind::Executable | object::ObjectKind::Dynamic
    ) {
        return Err(FbuildError::BuildFailed(
            "Static references require a final executable/shared ELF, not a relocatable object."
                .into(),
        ));
    }
    // AVR uses 16-bit ABI pointers even though its ELF container is ELF32.
    let width = if file.architecture() == object::Architecture::Avr {
        2
    } else if file.is_64() {
        8
    } else {
        4
    };
    let normalize = |address: u64| {
        if file.architecture() == object::Architecture::Arm {
            address & !1
        } else {
            address
        }
    };
    let functions = function_symbols(&file, normalize)?;
    let mut edges = Vec::new();
    for symbol in file.symbols() {
        let Ok(name) = symbol.name() else { continue };
        if !name.starts_with("_ZTV") || !symbol.is_definition() || symbol.size() == 0 {
            continue;
        }
        let Some(index) = symbol.section_index() else {
            continue;
        };
        let section = file
            .section_by_index(index)
            .map_err(|e| FbuildError::BuildFailed(e.to_string()))?;
        if !matches!(section.flags(), object::SectionFlags::Elf { sh_flags } if sh_flags & u64::from(object::elf::SHF_ALLOC) != 0)
        {
            continue;
        }
        let Some(start) = symbol.address().checked_sub(section.address()) else {
            continue;
        };
        let data = section
            .data()
            .map_err(|e| FbuildError::BuildFailed(e.to_string()))?;
        let Some(end) = start.checked_add(symbol.size()) else {
            continue;
        };
        let (Ok(start), Ok(end)) = (usize::try_from(start), usize::try_from(end)) else {
            continue;
        };
        let Some(table) = data.get(start..end) else {
            continue;
        };
        // Itanium ABI: the first slots are offset-to-top and RTTI, not methods.
        for (slot, word) in table.chunks_exact(width).enumerate().skip(2) {
            let pointer = read_pointer(word, file.is_little_endian());
            if pointer == 0 {
                continue;
            }
            // AVR program pointers address words; ELF function symbols address bytes.
            let address = if file.architecture() == object::Architecture::Avr {
                let Some(address) = pointer.checked_mul(2) else {
                    continue;
                };
                address
            } else {
                normalize(pointer)
            };
            if let Some(targets) = functions.get(&address) {
                for target in targets {
                    edges.push(StaticDataReference {
                        source_name: name.to_owned(),
                        source_address: symbol.address(),
                        target_name: target.clone(),
                        target_address: address,
                        offset: (slot * width) as u64,
                    });
                }
            }
        }
    }
    edges.sort_by(|a, b| {
        (&a.source_name, a.source_address, a.offset, &a.target_name).cmp(&(
            &b.source_name,
            b.source_address,
            b.offset,
            &b.target_name,
        ))
    });
    edges.dedup();
    Ok(edges)
}

fn function_symbols(
    file: &object::File<'_>,
    normalize: impl Fn(u64) -> u64,
) -> Result<BTreeMap<u64, Vec<String>>> {
    let mut functions: BTreeMap<u64, Vec<String>> = BTreeMap::new();
    for symbol in file.symbols() {
        if symbol.is_definition() && symbol.kind() == object::SymbolKind::Text {
            let Some(index) = symbol.section_index() else {
                continue;
            };
            let section = file
                .section_by_index(index)
                .map_err(|e| FbuildError::BuildFailed(e.to_string()))?;
            let executable = u64::from(object::elf::SHF_ALLOC | object::elf::SHF_EXECINSTR);
            if !matches!(section.flags(), object::SectionFlags::Elf { sh_flags } if sh_flags & executable == executable)
            {
                continue;
            }
            if let Ok(name) = symbol.name() {
                functions
                    .entry(normalize(symbol.address()))
                    .or_default()
                    .push(name.to_owned());
            }
        }
    }
    Ok(functions)
}

fn read_pointer(word: &[u8], little: bool) -> u64 {
    if little {
        word.iter().rev().fold(0, |n, b| (n << 8) | u64::from(*b))
    } else {
        word.iter().fold(0, |n, b| (n << 8) | u64::from(*b))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use object::write::{Object as WriteObject, Symbol, SymbolSection};
    use object::{
        Architecture, BinaryFormat, Endianness, SectionKind, SymbolFlags, SymbolKind, SymbolScope,
    };

    fn fixture(architecture: Architecture, endian: Endianness, width: usize) -> Vec<u8> {
        let mut elf = WriteObject::new(BinaryFormat::Elf, architecture, endian);
        let text = elf.add_section(Vec::new(), b".text".to_vec(), SectionKind::Text);
        elf.append_section_data(text, &[0; 32], 4);
        let data = elf.add_section(Vec::new(), b".rodata".to_vec(), SectionKind::ReadOnlyData);
        let mut pointers = Vec::new();
        let address = if architecture == Architecture::Avr {
            8_u64
        } else if architecture == Architecture::Arm {
            17_u64
        } else {
            16_u64
        };
        let encoded = if endian == Endianness::Little {
            address.to_le_bytes()
        } else {
            address.to_be_bytes()
        };
        // Even address-shaped header words must not be interpreted as methods.
        for _ in 0..3 {
            pointers.extend_from_slice(if endian == Endianness::Little {
                &encoded[..width]
            } else {
                &encoded[8 - width..]
            });
        }
        elf.append_section_data(data, &pointers, width as u64);
        for (name, value, size, kind, section) in [
            ("method", 16, 8, SymbolKind::Text, text),
            ("method_alias", 16, 8, SymbolKind::Text, text),
            ("_ZTV1A", 0, pointers.len() as u64, SymbolKind::Data, data),
            (
                "ordinary_scalar",
                0,
                pointers.len() as u64,
                SymbolKind::Data,
                data,
            ),
        ] {
            elf.add_symbol(Symbol {
                name: name.as_bytes().to_vec(),
                value,
                size,
                kind,
                scope: SymbolScope::Linkage,
                weak: false,
                section: SymbolSection::Section(section),
                flags: SymbolFlags::None,
            });
        }
        let mut bytes = elf.write().unwrap();
        // Raw final-image fixture: writer emits ET_REL, so mark this explicit
        // address fixture ET_EXEC. It has no unresolved relocations.
        bytes[16..18].copy_from_slice(if endian == Endianness::Little {
            &[2, 0]
        } else {
            &[0, 2]
        });
        bytes
    }

    #[test]
    fn vtable_edges_are_typed_and_preserve_aliases_across_width_and_endian() {
        for (architecture, endian, width) in [
            (Architecture::I386, Endianness::Little, 4),
            (Architecture::X86_64, Endianness::Little, 8),
            (Architecture::PowerPc, Endianness::Big, 4),
            (Architecture::PowerPc64, Endianness::Big, 8),
            (Architecture::Arm, Endianness::Little, 4),
            (Architecture::Avr, Endianness::Little, 2),
        ] {
            let edges = references_from_bytes(&fixture(architecture, endian, width)).unwrap();
            assert_eq!(edges.len(), 2, "{architecture:?}");
            assert!(edges.iter().all(|edge| edge.source_name == "_ZTV1A"
                && edge.target_address == 16
                && edge.offset == 2 * width as u64));
        }
    }

    #[test]
    fn relocatable_vtable_bytes_are_not_claimed_as_final_image_references() {
        let mut bytes = fixture(Architecture::I386, Endianness::Little, 4);
        bytes[16..18].copy_from_slice(&[1, 0]);
        assert!(references_from_bytes(&bytes).is_err());
    }

    #[test]
    fn malformed_elf_is_not_silently_reported_as_an_empty_graph() {
        assert!(references_from_bytes(b"not an ELF").is_err());
    }
}

#[cfg(test)]
mod linked_tests {
    use super::*;

    #[test]
    #[ignore = "requires native C++ compiler c++ to link a real vtable fixture"]
    fn linked_cpp_vtable_retains_method_through_static_pointer() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("vtable.cpp");
        let binary = dir.path().join("vtable.elf");
        std::fs::write(
            &source,
            r#"
            struct A { virtual int value(); };
            int A::value() { return 42; }
            A instance;
            int main() { return instance.value(); }
        "#,
        )
        .unwrap();
        let output = fbuild_core::subprocess::run_command_blocking(
            &[
                "c++",
                "-fno-pie",
                "-no-pie",
                "-fno-rtti",
                "-o",
                binary.to_str().unwrap(),
                source.to_str().unwrap(),
            ],
            Some(dir.path()),
            None,
            Some(std::time::Duration::from_secs(30)),
        )
        .unwrap();
        assert!(output.success(), "{}", output.stderr);
        let edges = read_vtable_references(&binary).unwrap();
        assert!(edges.iter().any(|edge| edge.source_name == "_ZTV1A"
            && edge.target_name == "_ZN1A5valueEv"
            && edge.offset == 16));
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use fbuild_core::symbol_analysis::{AnalysisStatus, ReferenceKind};

    #[tokio::test]
    #[ignore = "requires native C++ compiler c++, nm, c++filt and objdump"]
    async fn linked_weak_vtable_has_verified_incoming_edge_in_final_report() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("virtual.cpp");
        let binary = dir.path().join("virtual.elf");
        std::fs::write(
            &source,
            r#"
            struct A { virtual int value(); };
            int A::value() { return 42; }
            A instance;
            int main() { return instance.value(); }
        "#,
        )
        .unwrap();
        let output = fbuild_core::subprocess::run_command_blocking(
            &[
                "c++",
                "-fno-pie",
                "-no-pie",
                "-fno-rtti",
                "-o",
                binary.to_str().unwrap(),
                source.to_str().unwrap(),
            ],
            Some(dir.path()),
            None,
            Some(std::time::Duration::from_secs(30)),
        )
        .unwrap();
        assert!(output.success(), "{}", output.stderr);
        let report = super::super::analyze_elf(super::super::AnalyzeConfig {
            elf_path: &binary,
            map_path: None,
            nm_path: Path::new("nm"),
            cppfilt_path: Some(Path::new("c++filt")),
            objdump_path: Some(Path::new("objdump")),
        })
        .await
        .unwrap();
        let vtable = report
            .symbols
            .iter()
            .find(|symbol| symbol.mangled == "_ZTV1A")
            .expect("allocated weak vtable must be included");
        let method = report
            .symbols
            .iter()
            .find(|symbol| symbol.mangled == "_ZN1A5valueEv")
            .unwrap();
        assert!(vtable.size > 0);
        let analysis = &report.reference_analysis;
        assert_eq!(analysis.disassembly.status, AnalysisStatus::Analyzed);
        assert_eq!(analysis.static_data.status, AnalysisStatus::Analyzed);
        assert!(
            analysis
                .edges
                .iter()
                .any(|edge| edge.kind == ReferenceKind::StaticData
                    && edge.source.name == vtable.mangled
                    && edge.source.address == vtable.address
                    && edge.target.name == method.mangled
                    && edge.target.address == method.address
                    && edge.offset == Some(16))
        );
        assert!(analysis.roots.iter().any(|root| root.kind == "entry_point"));
        assert!(
            !analysis
                .unexplained
                .iter()
                .any(|symbol| symbol.name == method.mangled && symbol.address == method.address)
        );
        assert_partial_analysis(&binary, &report, &method.mangled, dir.path()).await;
    }

    async fn assert_partial_analysis(
        binary: &Path,
        report: &fbuild_core::symbol_analysis::FineGrainedSymbolMap,
        method: &str,
        dir: &Path,
    ) {
        let missing = dir.join("missing-objdump");
        for (tool, expected) in [
            (None, AnalysisStatus::Unavailable),
            (Some(missing.as_path()), AnalysisStatus::Error),
        ] {
            let partial = super::super::analyze_elf(super::super::AnalyzeConfig {
                elf_path: binary,
                map_path: None,
                nm_path: Path::new("nm"),
                cppfilt_path: Some(Path::new("c++filt")),
                objdump_path: tool,
            })
            .await
            .unwrap();
            assert_eq!(partial.reference_analysis.disassembly.status, expected);
            assert!(partial.reference_analysis.disassembly.reason.is_some());
            assert_eq!(
                partial.reference_analysis.static_data.status,
                AnalysisStatus::Analyzed
            );
            assert_eq!(partial.total_flash, report.total_flash);
            assert_eq!(partial.total_ram, report.total_ram);
            assert_eq!(partial.image_flash, report.image_flash);
            assert!(
                partial.reference_analysis.edges.iter().any(|edge| edge.kind
                    == ReferenceKind::StaticData
                    && edge.target.name == method)
            );
        }
    }
}
