//! Parser for GNU `ld --cref` Cross Reference Table sections of a map file.
//!
//! `ld` emits the cref block at the end of the map file. Each top-level
//! entry has the form:
//!
//! ```text
//! Cross Reference Table
//!
//! Symbol                                            File
//! _vfprintf_r                                       libc.a(libc_a-vfprintf.o)
//!                                                   libc.a(libc_a-vprintf.o)
//!                                                   libc.a(libc_a-printf.o)
//! ```
//!
//! The first file is the translation unit that defines the symbol;
//! every subsequent indented file is a translation unit that references
//! it. We store only the **referencers** under `referenced_by` so a
//! bloat report can answer "why was this symbol linked?" without the
//! definer noise (which is already on the row itself as `archive` +
//! `object`).
//!
//! cref granularity is **archive-member (`.o`), not symbol** — that's a
//! property of `ld --cref` itself, not a fbuild limitation. Consumers
//! shouldn't expect per-symbol back-references.
//!
//! Symbol names in the cref table are *mangled* (the linker doesn't
//! demangle for `--cref`). We key the returned map on the mangled name
//! so callers can join against `FineGrainedSymbol::mangled`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::extract_archive_and_object;

/// A back-reference parsed from the linker `--cref` table.
///
/// `archive` is `None` when the referencing TU is a bare object file on
/// disk (e.g. `src/main.cpp.o`). `object` is the archive member name
/// (`libc_a-vprintf.o`) or the bare object's basename.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SymbolReference {
    pub archive: Option<String>,
    pub object: String,
}

/// Parse the `Cross Reference Table` block of a GNU `ld` map file.
///
/// Returns a `mangled_symbol -> Vec<SymbolReference>` map of
/// *referencers* (the defining TU is intentionally excluded — that
/// information is already carried by the symbol row's own `archive`
/// + `object` fields). Returns an empty map when:
///
/// - the `Cross Reference Table` header is absent (older `ld`, or
///   the build ran with `-Wl,--no-cref`);
/// - the map file is well-formed but lists no symbols in the table.
///
/// Never panics on malformed input — best-effort parsing yields fewer
/// back-references rather than a hard error. The downstream contract
/// in [`super::FineGrainedSymbol::referenced_by`] is "empty vec means
/// no information", not "empty vec means error".
pub fn parse_cref_table(map_text: &str) -> BTreeMap<String, Vec<SymbolReference>> {
    let mut out: BTreeMap<String, Vec<SymbolReference>> = BTreeMap::new();
    let mut lines = map_text.lines();

    // Walk to the cref header.
    let mut found_header = false;
    for line in lines.by_ref() {
        if line.trim_start().starts_with("Cross Reference Table") {
            found_header = true;
            break;
        }
    }
    if !found_header {
        return out;
    }
    // Skip blank lines + the "Symbol ... File" column header.
    let mut saw_columns = false;
    for line in lines.by_ref() {
        let t = line.trim_start();
        if t.is_empty() {
            continue;
        }
        if t.starts_with("Symbol") {
            saw_columns = true;
            break;
        }
        // No column header — older emitters sometimes omit it. Treat
        // this line as the first symbol row.
        process_first_line(line, &mut out);
        break;
    }
    if !saw_columns && out.is_empty() {
        // We may have consumed nothing usable above; fall through to
        // the body-loop in case the iterator still has rows.
    }

    let mut current: Option<String> = None;
    let mut current_has_definer = false;
    for raw in lines {
        if raw.trim().is_empty() {
            // Blank line between groups — end the current group but
            // keep walking the table.
            current = None;
            current_has_definer = false;
            continue;
        }
        if raw.starts_with(char::is_whitespace) {
            // Continuation: file path only.
            let path = raw.trim();
            if path.is_empty() {
                continue;
            }
            let Some(sym) = current.as_ref() else {
                continue;
            };
            if !current_has_definer {
                // First file under the symbol is the defining TU;
                // skip it (the row already carries that attribution).
                current_has_definer = true;
                continue;
            }
            let (archive, object) = extract_archive_and_object(path);
            push_referencer(&mut out, sym, SymbolReference { archive, object });
            continue;
        }
        // New symbol row.
        let (sym, definer_seen) = process_symbol_row(raw, &mut out);
        current = Some(sym);
        current_has_definer = definer_seen;
    }

    out
}

/// Same as [`parse_cref_table`]'s symbol-row branch, but standalone for
/// the edge case where the cref block is missing its `Symbol ... File`
/// column header line.
fn process_first_line(raw: &str, out: &mut BTreeMap<String, Vec<SymbolReference>>) {
    let _ = process_symbol_row(raw, out);
}

/// Handle a non-indented symbol row. Returns `(symbol_name,
/// definer_seen_on_same_line)`. The symbol always gets a (possibly
/// still-empty) entry in `out` so subsequent indented referencer rows
/// have a key to attach to.
fn process_symbol_row(
    raw: &str,
    out: &mut BTreeMap<String, Vec<SymbolReference>>,
) -> (String, bool) {
    let mut parts = raw.splitn(2, char::is_whitespace);
    let symbol = parts.next().unwrap_or("").to_string();
    if symbol.is_empty() {
        return (String::new(), false);
    }
    let rest = parts.next().unwrap_or("").trim();
    out.entry(symbol.clone()).or_default();
    // The first file on this line (if any) is the definer — we
    // deliberately do NOT add it to `referenced_by`.
    let definer_seen = !rest.is_empty();
    (symbol, definer_seen)
}

fn push_referencer(
    out: &mut BTreeMap<String, Vec<SymbolReference>>,
    sym: &str,
    candidate: SymbolReference,
) {
    let refs = out.entry(sym.to_string()).or_default();
    if !refs.contains(&candidate) {
        refs.push(candidate);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refr(archive: Option<&str>, object: &str) -> SymbolReference {
        SymbolReference {
            archive: archive.map(|s| s.to_string()),
            object: object.to_string(),
        }
    }

    #[test]
    fn parse_returns_empty_when_no_cref_header() {
        let text = "\
Linker script and memory map

.flash.text     0x42000020    0x4026c
 .text.foo      0x42000020       0x10 path/libFastLED.a(foo.cpp.o)
";
        assert!(parse_cref_table(text).is_empty());
    }

    #[test]
    fn parse_basic_inline_layout() {
        // The dominant shape: definer + file on the same line as the
        // symbol name, referencers on indented continuation lines.
        let text = "\
Cross Reference Table

Symbol                                            File
_vfprintf_r                                       /tools/libc.a(libc_a-vfprintf.o)
                                                  /tools/libc.a(libc_a-vprintf.o)
                                                  /tools/libc.a(libc_a-printf.o)
                                                  /tools/libc.a(libc_a-fprintf.o)
vprintf                                           /tools/libc.a(libc_a-vprintf.o)
                                                  /tools/liblog.a(log_write.c.obj)
";
        let map = parse_cref_table(text);
        let vfprintf = map.get("_vfprintf_r").expect("_vfprintf_r missing");
        // Definer (vfprintf.o) excluded; referencers only.
        assert_eq!(
            vfprintf,
            &vec![
                refr(Some("libc.a"), "libc_a-vprintf.o"),
                refr(Some("libc.a"), "libc_a-printf.o"),
                refr(Some("libc.a"), "libc_a-fprintf.o"),
            ]
        );
        let vprintf = map.get("vprintf").expect("vprintf missing");
        assert_eq!(vprintf, &vec![refr(Some("liblog.a"), "log_write.c.obj")]);
    }

    #[test]
    fn parse_split_definer_layout() {
        // Long symbol name pushes the definer file onto the next
        // indented line. The first indented file is still the definer
        // — must be skipped.
        let text = "\
Cross Reference Table

Symbol                                            File
_ZNVeryLongMangledSymbolNameThatBlowsThroughTheCrefColumn
                                                  /tools/libc.a(libc_a-vfprintf.o)
                                                  /tools/libc.a(libc_a-vprintf.o)
                                                  /tools/libc.a(libc_a-printf.o)
";
        let map = parse_cref_table(text);
        let entry = map
            .get("_ZNVeryLongMangledSymbolNameThatBlowsThroughTheCrefColumn")
            .expect("symbol missing");
        // Three indented lines: first is the definer, last two are
        // the only referencers we should keep.
        assert_eq!(
            entry,
            &vec![
                refr(Some("libc.a"), "libc_a-vprintf.o"),
                refr(Some("libc.a"), "libc_a-printf.o"),
            ]
        );
    }

    #[test]
    fn parse_bare_object_referencer_has_no_archive() {
        // App-side referencers are bare `.o` paths with no archive.
        let text = "\
Cross Reference Table

Symbol                                            File
FastLED_ctor                                      .pio/build/esp32s3/lib0d9/libFastLED.a(fl.cpp.o)
                                                  .pio/build/esp32s3/src/main.cpp.o
";
        let map = parse_cref_table(text);
        let entry = map.get("FastLED_ctor").expect("symbol missing");
        assert_eq!(entry, &vec![refr(None, "main.cpp.o")]);
    }

    #[test]
    fn parse_deduplicates_repeated_referencers() {
        // Same TU referencing the symbol from multiple sites only
        // appears once in cref output, but defend against malformed
        // duplicates anyway.
        let text = "\
Cross Reference Table

Symbol                                            File
printf                                            /tools/libc.a(libc_a-printf.o)
                                                  /tools/libmbedcrypto.a(sha512.c.obj)
                                                  /tools/libmbedcrypto.a(sha512.c.obj)
                                                  /tools/libheap.a(tlsf.c.obj)
";
        let map = parse_cref_table(text);
        let entry = map.get("printf").expect("printf missing");
        assert_eq!(
            entry,
            &vec![
                refr(Some("libmbedcrypto.a"), "sha512.c.obj"),
                refr(Some("libheap.a"), "tlsf.c.obj"),
            ]
        );
    }

    #[test]
    fn parse_groups_separated_by_blank_lines() {
        // Some emitters insert a blank line between groups; we must
        // not lose subsequent symbols.
        let text = "\
Cross Reference Table

Symbol                                            File
foo                                               libA.a(foo.o)
                                                  libB.a(uses_foo.o)

bar                                               libA.a(bar.o)
                                                  libC.a(uses_bar.o)
";
        let map = parse_cref_table(text);
        assert_eq!(
            map.get("foo").unwrap(),
            &vec![refr(Some("libB.a"), "uses_foo.o")]
        );
        assert_eq!(
            map.get("bar").unwrap(),
            &vec![refr(Some("libC.a"), "uses_bar.o")]
        );
    }

    #[test]
    fn parse_symbol_with_no_referencers_yields_empty_vec() {
        // A symbol that's defined but never referenced (it's reachable
        // via some other root, e.g. `KEEP()` in the linker script).
        let text = "\
Cross Reference Table

Symbol                                            File
__StackTop                                        firmware.ld
";
        let map = parse_cref_table(text);
        // We still record the key so callers can distinguish "in the
        // cref but unreferenced" from "absent from cref entirely".
        // Either is rendered as "no referencers" in the report.
        let entry = map.get("__StackTop").expect("__StackTop missing");
        assert!(entry.is_empty(), "got {entry:?}");
    }
}
