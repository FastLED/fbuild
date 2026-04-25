//! In-process ELF file probe for test assertions.
//!
//! `ElfProbe` reads an ELF binary into memory and exposes section sizes and
//! symbol-table queries without shelling out to `nm`/`size`/`readelf`. It is
//! designed for per-board acceptance tests that assert ELF contents (e.g.,
//! `.bss <= 3 KB`, no `fnet_*`/`mbedtls_*` symbols on Blink targets).
//!
//! Parsing uses the [`object`] crate. The probe owns the bytes and re-parses
//! on every call; `object` is zero-copy, so the cost is negligible for tests.
//!
//! # Example
//!
//! ```no_run
//! use fbuild_test_support::ElfProbe;
//!
//! let probe = ElfProbe::open("firmware.elf").unwrap();
//! assert!(probe.section_size(".bss").unwrap() <= 3 * 1024);
//! assert!(!probe.has_symbol_containing("RadioHead").unwrap());
//! ```
//!
//! Demangling is intentionally out of scope. `has_symbol_containing` matches
//! against raw (possibly mangled) names — searching for a class or namespace
//! substring such as "RadioHead" or "mbedtls" is sufficient for the
//! presence/absence checks in #205 acceptance criteria.

use std::fs;
use std::path::Path;

use object::{Object, ObjectSection, ObjectSymbol};

/// In-memory ELF file probe. See module docs.
#[derive(Debug, Clone)]
pub struct ElfProbe {
    bytes: Vec<u8>,
}

/// Information about a single ELF section.
#[derive(Debug, Clone)]
pub struct SectionInfo {
    /// Section name (e.g., `.text`, `.bss`).
    pub name: String,
    /// Section size in bytes (`sh_size`).
    pub size: u64,
    /// Virtual address of the section (`sh_addr`).
    pub address: u64,
}

/// Information about a single ELF symbol.
#[derive(Debug, Clone)]
pub struct SymbolInfo {
    /// Symbol name (raw / mangled — no demangling).
    pub name: String,
    /// Symbol size in bytes (`st_size`).
    pub size: u64,
    /// Symbol address (`st_value`).
    pub address: u64,
    /// True if the symbol is undefined (i.e., refers to an external).
    pub is_undefined: bool,
}

/// Errors returned from [`ElfProbe`] operations.
#[derive(Debug, thiserror::Error)]
pub enum ElfProbeError {
    /// Filesystem I/O failed (e.g., file missing, permission denied).
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// Parsing the ELF bytes failed.
    #[error("parse: {0}")]
    Parse(String),
}

impl ElfProbe {
    /// Read and memoize the ELF bytes for repeated probing.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, ElfProbeError> {
        let bytes = fs::read(path)?;
        Ok(Self { bytes })
    }

    /// Construct from an in-memory buffer (for fixtures).
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }

    /// Every named section, including zero-sized ones, in original ELF
    /// section-header order. Unnamed (empty-name) sections are skipped.
    ///
    /// Zero-sized sections are kept so callers that want a faithful section
    /// list (e.g. "is `.bss` present at all?") get the right answer.
    /// `section_size(name)` still returns 0 when the section is absent.
    pub fn sections(&self) -> Result<Vec<SectionInfo>, ElfProbeError> {
        let file = self.parse()?;
        let mut out = Vec::new();
        for section in file.sections() {
            let Ok(name) = section.name() else { continue };
            if name.is_empty() {
                continue;
            }
            out.push(SectionInfo {
                name: name.to_string(),
                size: section.size(),
                address: section.address(),
            });
        }
        Ok(out)
    }

    /// Look up a section by exact name. Returns `None` if absent.
    pub fn section(&self, name: &str) -> Result<Option<SectionInfo>, ElfProbeError> {
        Ok(self.sections()?.into_iter().find(|s| s.name == name))
    }

    /// Convenience: just the size in bytes, or 0 if section is absent.
    pub fn section_size(&self, name: &str) -> Result<u64, ElfProbeError> {
        Ok(self.section(name)?.map(|s| s.size).unwrap_or(0))
    }

    /// Every symbol in the static `.symtab`, in symbol-table order.
    ///
    /// Demangling is out of scope — names are returned exactly as stored.
    pub fn symbols(&self) -> Result<Vec<SymbolInfo>, ElfProbeError> {
        let file = self.parse()?;
        let mut out = Vec::new();
        for sym in file.symbols() {
            let Ok(name) = sym.name() else { continue };
            if name.is_empty() {
                continue;
            }
            out.push(SymbolInfo {
                name: name.to_string(),
                size: sym.size(),
                address: sym.address(),
                is_undefined: sym.is_undefined(),
            });
        }
        Ok(out)
    }

    /// Whether ANY symbol has the given exact name.
    pub fn has_symbol(&self, name: &str) -> Result<bool, ElfProbeError> {
        Ok(self.symbols()?.iter().any(|s| s.name == name))
    }

    /// Whether ANY symbol's name CONTAINS the given substring.
    ///
    /// Useful for spotting mangled C++ names by class/namespace — e.g.,
    /// `has_symbol_containing("RadioHead")` matches `_ZN8RadioHead4sendEv`.
    pub fn has_symbol_containing(&self, needle: &str) -> Result<bool, ElfProbeError> {
        Ok(self.symbols()?.iter().any(|s| s.name.contains(needle)))
    }

    /// Sum of `.text + .data + .bss`. Missing sections contribute 0.
    pub fn text_data_bss_sum(&self) -> Result<u64, ElfProbeError> {
        let mut total: u64 = 0;
        for name in [".text", ".data", ".bss"] {
            total = total.saturating_add(self.section_size(name)?);
        }
        Ok(total)
    }

    /// Parse the bytes into a generic `object::File`. Errors surface as
    /// [`ElfProbeError::Parse`].
    fn parse(&self) -> Result<object::File<'_>, ElfProbeError> {
        // Reject anything that doesn't start with an ELF magic; `object` will
        // happily parse Mach-O / PE / etc. otherwise.
        if self.bytes.len() < 4 || &self.bytes[..4] != b"\x7fELF" {
            return Err(ElfProbeError::Parse("not an ELF file".to_string()));
        }
        object::File::parse(self.bytes.as_slice()).map_err(|e| ElfProbeError::Parse(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use object::elf;
    use object::write::elf::{FileHeader, SectionHeader, Sym, Writer};
    use object::write::StringId;
    use object::Endianness;

    /// Build a minimal little-endian 32-bit ARM ELF executable with the named
    /// sections (each filled with `size` zero bytes so `.text/.data` contribute
    /// real bytes; `.bss` is special-cased as `SHT_NOBITS`) and the named
    /// global symbols. Returns an in-memory ELF byte buffer suitable for
    /// `ElfProbe::from_bytes`.
    ///
    /// All sections are placed contiguously in memory starting at vaddr 0x1000.
    fn build_elf(sections: &[(&str, u64)], symbols: &[(&str, u64)]) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut writer = Writer::new(Endianness::Little, false, &mut buf);

        // Reserve indices ----------------------------------------------------
        writer.reserve_file_header();

        // One null section + one per requested section + .symtab + .strtab + .shstrtab.
        // We let the Writer manage symtab/strtab/shstrtab sections itself.
        let section_ids: Vec<_> = (0..sections.len())
            .map(|_| writer.reserve_section_index())
            .collect();
        let section_names: Vec<StringId> = sections
            .iter()
            .map(|(name, _)| writer.add_section_name(name.as_bytes()))
            .collect();

        writer.reserve_null_symbol_index();
        let mut sym_entries: Vec<(StringId, u64, usize)> = Vec::new();
        for (sym_name, _) in symbols {
            let _ = writer.reserve_symbol_index(None);
            let name_id = writer.add_string(sym_name.as_bytes());
            // Place symbols inside the first section, address 0 within it.
            sym_entries.push((name_id, 0, 0));
        }
        writer.reserve_symtab_section_index();
        writer.reserve_strtab_section_index();
        writer.reserve_shstrtab_section_index();

        // Reserve file offsets ----------------------------------------------
        // Section data: `.bss` is NOBITS (no file space). Others get `size` bytes.
        let mut section_offsets: Vec<u64> = Vec::with_capacity(sections.len());
        for (name, size) in sections {
            if *name == ".bss" {
                section_offsets.push(0);
            } else {
                let offset = writer.reserve(*size as usize, 1) as u64;
                section_offsets.push(offset);
            }
        }
        writer.reserve_symtab();
        writer.reserve_strtab();
        writer.reserve_shstrtab();
        writer.reserve_section_headers();

        // Write file header --------------------------------------------------
        writer
            .write_file_header(&FileHeader {
                os_abi: elf::ELFOSABI_NONE,
                abi_version: 0,
                e_type: elf::ET_REL,
                e_machine: elf::EM_ARM,
                e_entry: 0,
                e_flags: 0,
            })
            .expect("write file header");

        // Write section data -------------------------------------------------
        for (i, (name, size)) in sections.iter().enumerate() {
            if *name == ".bss" {
                continue;
            }
            writer.pad_until(section_offsets[i] as usize);
            writer.write(&vec![0u8; *size as usize]);
        }

        // Symbol table (entry 0 is null and written automatically) ----------
        writer.write_null_symbol();
        for (sym_idx, (sym_name, _)) in symbols.iter().enumerate() {
            let (name_id, _addr, _section_idx) = sym_entries[sym_idx];
            // Bind in the first defined section if any, else SHN_ABS.
            let st_shndx = if sections.is_empty() {
                elf::SHN_ABS
            } else {
                section_ids[0].0 as u16
            };
            writer.write_symbol(&Sym {
                name: Some(name_id),
                section: if sections.is_empty() {
                    None
                } else {
                    Some(section_ids[0])
                },
                st_info: (elf::STB_GLOBAL << 4) | elf::STT_OBJECT,
                st_other: 0,
                st_shndx,
                st_value: 0,
                st_size: 0,
            });
            let _ = sym_name; // keep clippy happy — name is consumed via name_id
        }

        writer.write_strtab();
        writer.write_shstrtab();

        // Section headers ---------------------------------------------------
        writer.write_null_section_header();
        for (i, (name, size)) in sections.iter().enumerate() {
            let (sh_type, sh_flags): (u32, u32) = match *name {
                ".text" => (elf::SHT_PROGBITS, elf::SHF_ALLOC | elf::SHF_EXECINSTR),
                ".bss" => (elf::SHT_NOBITS, elf::SHF_ALLOC | elf::SHF_WRITE),
                ".data" => (elf::SHT_PROGBITS, elf::SHF_ALLOC | elf::SHF_WRITE),
                _ => (elf::SHT_PROGBITS, elf::SHF_ALLOC),
            };
            writer.write_section_header(&SectionHeader {
                name: Some(section_names[i]),
                sh_type,
                sh_flags: u64::from(sh_flags),
                sh_addr: 0x1000 + (i as u64) * 0x1000,
                sh_offset: section_offsets[i],
                sh_size: *size,
                sh_link: 0,
                sh_info: 0,
                sh_addralign: 1,
                sh_entsize: 0,
            });
        }
        writer.write_symtab_section_header(1); // first non-local symbol index
        writer.write_strtab_section_header();
        writer.write_shstrtab_section_header();

        buf
    }

    #[test]
    fn from_bytes_round_trip() {
        let bytes = build_elf(&[(".text", 4)], &[("setup", 0)]);
        let probe = ElfProbe::from_bytes(bytes.clone());
        assert_eq!(probe.bytes.len(), bytes.len());
    }

    #[test]
    fn sections_lists_text_data_bss_in_known_fixture() {
        let bytes = build_elf(
            &[(".text", 64), (".data", 16), (".bss", 32)],
            &[("setup", 0)],
        );
        let probe = ElfProbe::from_bytes(bytes);
        let sections = probe.sections().expect("sections");
        let by_name: std::collections::HashMap<_, _> =
            sections.iter().map(|s| (s.name.clone(), s.size)).collect();
        assert_eq!(by_name.get(".text").copied(), Some(64));
        assert_eq!(by_name.get(".data").copied(), Some(16));
        assert_eq!(by_name.get(".bss").copied(), Some(32));
    }

    #[test]
    fn section_size_returns_zero_for_missing() {
        let bytes = build_elf(&[(".text", 8)], &[]);
        let probe = ElfProbe::from_bytes(bytes);
        assert_eq!(probe.section_size(".dmabuffers").expect("size"), 0);
    }

    #[test]
    fn text_data_bss_sum_aggregates() {
        let bytes = build_elf(&[(".text", 100), (".data", 20), (".bss", 8)], &[]);
        let probe = ElfProbe::from_bytes(bytes);
        assert_eq!(probe.text_data_bss_sum().expect("sum"), 128);
    }

    #[test]
    fn text_data_bss_sum_handles_partial_sections() {
        let bytes = build_elf(&[(".text", 50)], &[]);
        let probe = ElfProbe::from_bytes(bytes);
        assert_eq!(probe.text_data_bss_sum().expect("sum"), 50);
    }

    #[test]
    fn symbols_lists_known_symbols() {
        let bytes = build_elf(
            &[(".text", 4)],
            &[("setup", 0), ("loop", 0), ("digitalWrite", 0)],
        );
        let probe = ElfProbe::from_bytes(bytes);
        assert!(probe.has_symbol("setup").expect("setup"));
        assert!(probe.has_symbol("loop").expect("loop"));
        assert!(probe.has_symbol("digitalWrite").expect("digitalWrite"));
        assert!(!probe.has_symbol("not_present").expect("missing"));
    }

    #[test]
    fn has_symbol_containing_handles_substring() {
        let bytes = build_elf(
            &[(".text", 4)],
            &[("_ZN8RadioHead4sendEv", 0), ("setup", 0)],
        );
        let probe = ElfProbe::from_bytes(bytes);
        assert!(probe.has_symbol_containing("RadioHead").expect("RadioHead"));
        assert!(!probe.has_symbol_containing("mbedtls").expect("mbedtls"));
    }

    #[test]
    fn non_elf_input_returns_parse_error() {
        let probe = ElfProbe::from_bytes(b"not an elf".to_vec());
        match probe.sections() {
            Err(ElfProbeError::Parse(_)) => {}
            other => panic!("expected Parse error, got {other:?}"),
        }
    }

    #[test]
    fn open_returns_io_error_for_missing_file() {
        let path = std::env::temp_dir().join("fbuild-elfprobe-no-such-file-xyz123.elf");
        // Ensure it really doesn't exist.
        let _ = std::fs::remove_file(&path);
        match ElfProbe::open(&path) {
            Err(ElfProbeError::Io(_)) => {}
            other => panic!("expected Io error, got {other:?}"),
        }
    }

    #[test]
    fn section_lookup_returns_none_when_absent() {
        let bytes = build_elf(&[(".text", 4)], &[]);
        let probe = ElfProbe::from_bytes(bytes);
        assert!(probe.section(".dmabuffers").expect("section").is_none());
        assert!(probe.section(".text").expect("text").is_some());
    }

    #[test]
    fn zero_sized_section_appears_in_listing() {
        // Adversary: a target with .bss legitimately sized 0. Old behaviour
        // (size > 0 filter) hid such sections from `sections()` and made
        // `section(".bss")` return None even though the section header was
        // present. The new contract returns every named section regardless
        // of size — `section(".bss")` returns Some with size=0.
        let bytes = build_elf(&[(".text", 16), (".bss", 0)], &[]);
        let probe = ElfProbe::from_bytes(bytes);
        let sections = probe.sections().expect("sections");
        assert!(
            sections.iter().any(|s| s.name == ".bss" && s.size == 0),
            "expected zero-sized .bss in listing, got {:?}",
            sections
        );
        let bss = probe.section(".bss").expect("bss query");
        assert_eq!(bss.map(|s| s.size), Some(0));
        // section_size still returns 0 either way.
        assert_eq!(probe.section_size(".bss").expect("size"), 0);
    }

    #[test]
    fn truncated_elf_returns_parse_error() {
        // Adversary: ELF magic present but body truncated to under the
        // smallest viable header. Must not panic — should surface a Parse
        // error.
        let bytes = b"\x7fELF\x00\x00\x00".to_vec();
        let probe = ElfProbe::from_bytes(bytes);
        match probe.sections() {
            Err(ElfProbeError::Parse(_)) => {}
            other => panic!("expected Parse error, got {other:?}"),
        }
    }

    #[test]
    fn empty_input_returns_parse_error() {
        let probe = ElfProbe::from_bytes(Vec::new());
        match probe.sections() {
            Err(ElfProbeError::Parse(_)) => {}
            other => panic!("expected Parse error, got {other:?}"),
        }
    }

    #[test]
    fn missing_symbol_returns_false() {
        let bytes = build_elf(&[(".text", 4)], &[("setup", 0x1000)]);
        let probe = ElfProbe::from_bytes(bytes);
        assert!(!probe.has_symbol("nonexistent_symbol").expect("query"));
        assert!(!probe.has_symbol_containing("nonexistent").expect("query"));
    }
}
