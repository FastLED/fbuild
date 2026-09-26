//! Linker-script marker detection (FastLED/fbuild#1456).
//!
//! Linker scripts define boundary labels such as `_stext`,
//! `_text_start` or `_bss_end` with assignments (`_stext = .;`,
//! `PROVIDE (UART0 = 0x60000000)`). They own no bytes: the ELF gives
//! them `st_size == 0`. Some `nm` builds (e.g. host GNU binutils 2.46
//! with `--size-sort`) nevertheless *synthesise* a size for them from
//! the gap to the next symbol, so a label at the start of
//! `.flash.text` shows up as a ~10 KB "symbol" that swallows the real
//! `.literal.*` / `.text.*` input sections behind it. That made
//! `total_flash` depend on which `nm` produced the rows.
//!
//! The linker map lists every script assignment in its layout view,
//! so the set of marker names is recovered from the map and those rows
//! are dropped before attribution.

use std::collections::BTreeSet;

/// Collect the names assigned by the linker script in the map's
/// "Linker script and memory map" section.
///
/// Recognised layout-view lines (leading whitespace, address first):
/// ```text
///                 0x42000020                        _stext = .
///                 0x42000020                        _text_start = ABSOLUTE (.)
///                 0x60000000                        PROVIDE (UART0 = 0x60000000)
/// ```
/// Location-counter assignments (`. = ALIGN (0x4)`) are ignored.
pub fn parse_linker_script_symbols(map_text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut in_link_view = false;
    for raw in map_text.lines() {
        if !in_link_view {
            in_link_view = raw.starts_with("Linker script and memory map");
            continue;
        }
        let mut it = raw.split_whitespace();
        let Some(addr) = it.next() else { continue };
        if !addr.starts_with("0x") {
            continue;
        }
        let rest: Vec<&str> = it.collect();
        let (name, eq) = match rest.as_slice() {
            ["PROVIDE", name, eq, ..] | ["PROVIDE_HIDDEN", name, eq, ..] => {
                (name.trim_start_matches('('), *eq)
            }
            [name, eq, ..] => (*name, *eq),
            _ => continue,
        };
        if eq != "=" || name == "." || name.is_empty() {
            continue;
        }
        out.insert(name.to_string());
    }
    out
}

/// Drop `nm` rows whose name is a linker-script marker.
pub fn strip_linker_markers(
    nm_rows: Vec<(u64, u64, char, String)>,
    markers: &BTreeSet<String>,
) -> Vec<(u64, u64, char, String)> {
    if markers.is_empty() {
        return nm_rows;
    }
    nm_rows
        .into_iter()
        .filter(|(_, _, _, name)| !markers.contains(name))
        .collect()
}

/// Drop `nm` rows for symbols whose ELF `st_size` is 0.
///
/// `zero_sized` holds `(address, name)` for every symbol-table entry with
/// `st_size == 0` (assembly labels such as `_WindowOverflow4`,
/// `_xt_interrupt_table`, and script labels). Such a symbol owns no
/// bytes; a size on its `nm` row was synthesised by `nm` itself, and
/// whether `nm` does that depends on the binutils build. Dropping the
/// rows makes attribution identical across `nm` versions; the bytes
/// behind them stay covered by map-derived rows where the input
/// section names an owner, and always by `image_flash`.
pub fn strip_unsized_symbols(
    nm_rows: Vec<(u64, u64, char, String)>,
    zero_sized: &BTreeSet<(u64, String)>,
) -> Vec<(u64, u64, char, String)> {
    if zero_sized.is_empty() {
        return nm_rows;
    }
    nm_rows
        .into_iter()
        .filter(|(addr, _, _, name)| !zero_sized.contains(&(*addr, name.clone())))
        .collect()
}
