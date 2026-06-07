//! Parser for `objdump -d` disassembly that extracts a per-symbol call
//! graph (forward references).
//!
//! Complements [`super::cref`], which gives us *back*-references
//! (TU-level granularity). For the forward direction we need
//! per-symbol precision — the cref data structure can't give us
//! "which symbols does `ClocklessIdf5` call?" because cref is keyed by
//! `.o`, not by symbol.
//!
//! We parse the textual disassembly format because it's
//! architecture-neutral and stable across binutils versions:
//!
//! ```text
//! 00400500 <ClocklessIdf5>:
//!   400500:   bl     400600 <esp_log_write>
//!   400504:   call4  400700 <FastLED::lerp8by8>
//!   400508:   jal    ra, 400800 <fl::sort>
//! ```
//!
//! Each function header has the form `<addr> <symbol>:`; subsequent
//! indented lines are instructions. We record an edge whenever an
//! instruction's textual tail ends in `<symbol_name>` (objdump
//! normalises this regardless of mnemonic: bl/b/call4/call8/jal/jalr/
//! tail/rcall/rjmp all emit the same `<...>` annotation).
//!
//! ## Why not relocations on .o files?
//!
//! `objdump -r` over each `.o` gives strictly-correct per-symbol
//! relocation entries. Two reasons we don't go that route:
//!
//! 1. fbuild deletes per-TU `.o` files after the link succeeds; the
//!    only artefact retained is the linked ELF.
//! 2. The relocation entries reflect the **pre-link** symbol graph.
//!    After LTO + `-Os` the linker collapses helpers into their
//!    callers; the relocation graph over-reports edges that no
//!    longer exist in the final binary.
//!
//! The textual disassembly of the final ELF is therefore *more*
//! accurate for "what's actually called in the shipped binary", at
//! the cost of being conservative — calls indirected through a
//! function pointer (e.g. through a vtable load + `blx r3`) show no
//! `<symbol>` annotation and are silently dropped. That mirrors the
//! `--cref` back-reference contract: listed edges are real, missing
//! edges may exist.

use std::collections::BTreeMap;

/// One edge: `caller` calls `callee`. Both names match
/// `FineGrainedSymbol::mangled` (objdump emits the mangled name in
/// the angle-bracket annotation when the binary hasn't been demangled
/// post-link; we tell the analyzer to invoke objdump without `-C` for
/// exactly this reason).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CallEdge {
    pub caller: String,
    pub callee: String,
}

/// Parse `objdump -d` output into a `caller -> Vec<callee>` map.
///
/// Skips:
///   - self-references (`f` calling `f` from a tail call back into
///     itself; not interesting for bloat analysis).
///   - PLT trampolines (callee name ends in `@plt`) — for size
///     analysis the actual import is what matters.
///   - the `$x`/`$d`/`$t` ARM mapping symbols (objdump injects these
///     between code and data; they're not real call targets).
///   - duplicate edges from the same caller (multiple calls to the
///     same callee collapse to one edge).
///
/// Always succeeds — malformed lines are silently skipped. The
/// downstream contract is "empty list = no information", not
/// "empty list = error".
#[must_use]
pub fn parse_disasm(disasm: &str) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut current: Option<String> = None;
    let mut seen: std::collections::BTreeSet<(String, String)> = std::collections::BTreeSet::new();

    for line in disasm.lines() {
        // 1. Function header: "<addr> <symbol_name>:"
        if let Some(name) = parse_function_header(line) {
            current = Some(name);
            continue;
        }
        // 2. Empty line resets nothing (a function body can contain
        //    blank lines between basic blocks on some platforms).
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            continue;
        }
        // 3. Section header: "Disassembly of section .text:" — clears
        //    current so we don't attribute the next blob to the wrong
        //    symbol.
        if trimmed.starts_with("Disassembly of section") {
            current = None;
            continue;
        }
        // 4. Instruction line: hex offset followed by mnemonic + args.
        let Some(caller) = current.as_deref() else {
            continue;
        };
        let Some(callee) = parse_call_target(line) else {
            continue;
        };
        if !is_real_call_target(&callee) || callee == caller {
            continue;
        }
        let key = (caller.to_string(), callee.clone());
        if seen.insert(key) {
            out.entry(caller.to_string()).or_default().push(callee);
        }
    }
    out
}

/// Parse a function header line. Returns the symbol name if the line
/// has the shape `<hex_addr> <name>:`. Lines like
/// `00400500 <ClocklessIdf5>:` produce `Some("ClocklessIdf5")`.
fn parse_function_header(line: &str) -> Option<String> {
    let trimmed = line.trim_end();
    // Must end with ":" and contain "<name>" preceded by a hex addr.
    let body = trimmed.strip_suffix(':')?;
    let (addr_part, name_part) = body.split_once(" <")?;
    // Address must be all hex digits (drop leading 0x/zero padding).
    if !addr_part
        .trim_start_matches("0x")
        .chars()
        .all(|c| c.is_ascii_hexdigit())
    {
        return None;
    }
    let name = name_part.strip_suffix('>')?;
    if name.is_empty() {
        return None;
    }
    Some(name.to_string())
}

/// Extract the call-target symbol name from an instruction line, if
/// any. Looks for the last `<...>` token (the symbol annotation
/// objdump appends to call/branch instructions). Returns the inner
/// name without the angle brackets.
///
/// Examples that yield `Some(...)`:
///   - `  400500:   bl     400600 <esp_log_write>`           → `esp_log_write`
///   - `  400500:   call8  400700 <FastLED::lerp8by8>`       → `FastLED::lerp8by8`
///   - `  400500:   jal    ra, 400800 <fl::sort>`            → `fl::sort`
///   - `  400500:   tail   <__udivdi3>`                      → `__udivdi3`
///
/// Examples that yield `None`:
///   - `  400500:   add    r0, r1, r2`                        (no `<...>`)
///   - `  400500:   blx    r3`                                (indirect, no symbol)
///   - `  400500:   .word  0x00000000`                        (data)
fn parse_call_target(line: &str) -> Option<String> {
    // We need *the last* `<...>` on the line because some encodings
    // emit two: e.g. `bl 400600 <baz>+0x10 <baz>` (rare but seen on
    // big binaries with --visualize-jumps disabled). The last one is
    // always the call target.
    let close = line.rfind('>')?;
    let before_close = &line[..close];
    let open = before_close.rfind('<')?;
    let inner = &line[open + 1..close];
    if inner.is_empty() {
        return None;
    }
    Some(inner.to_string())
}

/// Filter out tokens that look like symbols but aren't real call
/// targets for size analysis. The disassembler injects mapping
/// markers and PLT shims; we don't want those polluting the graph.
fn is_real_call_target(name: &str) -> bool {
    // ARM ELF mapping symbols. The toolchain inserts `$a` / `$t` /
    // `$d` at the boundary between ARM code, Thumb code, and data.
    if matches!(name, "$a" | "$t" | "$d") {
        return false;
    }
    // PLT shims. `printf@plt` is a trampoline; the real `printf`
    // lives elsewhere. For backref + forward attribution we want
    // the real callee, but objdump on a fully-linked ELF rarely
    // emits `@plt` annotations (no PLT in statically-linked
    // firmware). Filter conservatively.
    if name.ends_with("@plt") {
        return false;
    }
    // Hex literals dressed up as `<0x4040_1234>` (an absolute
    // address with no known symbol). These look like edges but
    // don't tell us anything useful.
    if name.starts_with("0x") && name[2..].chars().all(|c| c.is_ascii_hexdigit() || c == '_') {
        return false;
    }
    true
}

/// Build the inverse of [`parse_disasm`]'s output: `callee -> Vec<caller>`.
/// Useful when the caller wants to surface "who calls X?" via the
/// forward map (a redundant view to `--cref` back-references, but at
/// per-symbol granularity rather than TU).
#[must_use]
pub fn invert(forward: &BTreeMap<String, Vec<String>>) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut seen: std::collections::BTreeSet<(String, String)> = std::collections::BTreeSet::new();
    for (caller, callees) in forward {
        for callee in callees {
            let key = (callee.clone(), caller.clone());
            if seen.insert(key) {
                out.entry(callee.clone()).or_default().push(caller.clone());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_arm_thumb_bl_call() {
        let disasm = "\
00000500 <main>:
     500:	b500      	push	{lr}
     502:	f000 f800 	bl	506 <delay>
     506:	bd00      	pop	{pc}

00000506 <delay>:
     506:	4770      	bx	lr
";
        let edges = parse_disasm(disasm);
        assert_eq!(edges.get("main"), Some(&vec!["delay".to_string()]));
        // delay has no outgoing edges (bx lr is a return).
        assert!(!edges.contains_key("delay"));
    }

    /// Xtensa ESP32 `call4` / `call8` / `l32r` flavours.
    #[test]
    fn parses_xtensa_call() {
        let disasm = "\
40080000 <app_main>:
40080000:	004136        	entry	a1, 32
40080003:	0c0006        	call4	40080020 <esp_log_write>
40080006:	0c0006        	call8	40080040 <FastLED::lerp8by8>
40080009:	f00c          	retw.n
";
        let edges = parse_disasm(disasm);
        let calls = edges.get("app_main").expect("app_main has edges");
        assert!(calls.contains(&"esp_log_write".to_string()));
        assert!(calls.contains(&"FastLED::lerp8by8".to_string()));
    }

    /// RISC-V `jal` / `tail`.
    #[test]
    fn parses_riscv_jal_and_tail() {
        let disasm = "\
20000100 <my_task>:
20000100:	1101                	add	sp,sp,-32
20000102:	c606                	sw	a1,12(sp)
20000104:	2eb000ef          	jal	ra, 200001e8 <esp_log_write>
20000108:	2eb000ef          	tail	200001f0 <fl::sort>
2000010c:	8082                	ret
";
        let edges = parse_disasm(disasm);
        let calls = edges.get("my_task").expect("my_task has edges");
        assert!(calls.contains(&"esp_log_write".to_string()));
        assert!(calls.contains(&"fl::sort".to_string()));
    }

    /// AVR `call` / `rcall` / `jmp`.
    #[test]
    fn parses_avr_call_rcall() {
        let disasm = "\
00000080 <setup>:
  80:	0f 93       	push	r16
  82:	0e 94 c0 00 	call	0x180 <FastLED_init>
  86:	df cf       	rcall	.-66    	; 0x46 <delay_ms>
  88:	08 95       	ret
";
        let edges = parse_disasm(disasm);
        let calls = edges.get("setup").expect("setup has edges");
        assert!(calls.contains(&"FastLED_init".to_string()));
        assert!(calls.contains(&"delay_ms".to_string()));
    }

    /// Indirect calls (`blx r3`, `jalr` on a register) have no
    /// symbol annotation — they're silently dropped, matching the
    /// "missing edges may exist" contract.
    #[test]
    fn indirect_calls_are_dropped() {
        let disasm = "\
00000500 <vtable_dispatch>:
     500:	f8d0 3008 	ldr.w	r3, [r0, #8]
     504:	4798      	blx	r3
     506:	bd00      	pop	{pc}
";
        let edges = parse_disasm(disasm);
        assert!(!edges.contains_key("vtable_dispatch"));
    }

    /// Duplicate calls to the same callee collapse to a single edge.
    #[test]
    fn duplicate_callees_dedupe() {
        let disasm = "\
00000500 <loop>:
     500:	f7ff fffe 	bl	0 <step>
     504:	f7ff fffe 	bl	0 <step>
     508:	f7ff fffe 	bl	0 <step>
";
        let edges = parse_disasm(disasm);
        assert_eq!(edges.get("loop"), Some(&vec!["step".to_string()]));
    }

    /// Self-references (e.g. recursion) aren't edges for the
    /// purposes of bloat analysis.
    #[test]
    fn self_references_dropped() {
        let disasm = "\
00000500 <fib>:
     500:	f7ff fffe 	bl	500 <fib>
     504:	bd00      	pop	{pc}
";
        let edges = parse_disasm(disasm);
        assert!(!edges.contains_key("fib"));
    }

    /// ARM `$a`/`$t`/`$d` mapping markers must not appear as callees.
    #[test]
    fn arm_mapping_symbols_filtered() {
        let disasm = "\
00000500 <foo>:
     500:	f7ff fffe 	bl	520 <$a>
     504:	f7ff fffe 	bl	520 <$d>
     508:	f7ff fffe 	bl	520 <real_callee>
";
        let edges = parse_disasm(disasm);
        assert_eq!(edges.get("foo"), Some(&vec!["real_callee".to_string()]));
    }

    /// PLT trampoline annotations don't appear in fully statically
    /// linked firmware, but if a host-tool binary slipped through we
    /// don't want them polluting the graph.
    #[test]
    fn plt_callees_filtered() {
        let disasm = "\
00000500 <foo>:
     500:	f7ff fffe 	bl	520 <printf@plt>
     504:	f7ff fffe 	bl	520 <real_callee>
";
        let edges = parse_disasm(disasm);
        assert_eq!(edges.get("foo"), Some(&vec!["real_callee".to_string()]));
    }

    /// `<0x40123456>`-style absolute-address annotations (no known
    /// symbol) are noise — drop them.
    #[test]
    fn hex_address_annotations_filtered() {
        let disasm = "\
00000500 <foo>:
     500:	f7ff fffe 	bl	520 <0x40123456>
     504:	f7ff fffe 	bl	520 <real_callee>
";
        let edges = parse_disasm(disasm);
        assert_eq!(edges.get("foo"), Some(&vec!["real_callee".to_string()]));
    }

    /// `Disassembly of section .text:` headers (and similar) reset
    /// the current-function context so we don't attribute a stray
    /// instruction from a later section to the previous function.
    #[test]
    fn section_header_resets_current_function() {
        let disasm = "\
00000500 <foo>:
     500:	f7ff fffe 	bl	520 <foo_callee>

Disassembly of section .data:

     600:	f7ff fffe 	bl	520 <should_not_attach_to_foo>
";
        let edges = parse_disasm(disasm);
        assert_eq!(edges.get("foo"), Some(&vec!["foo_callee".to_string()]));
        assert!(!edges
            .values()
            .any(|callees| callees.contains(&"should_not_attach_to_foo".to_string())));
    }

    /// C++ mangled names contain `::`, `<>`, and parentheses. We
    /// must not split on them — the whole inner-bracket payload is
    /// the symbol name.
    #[test]
    fn cpp_mangled_names_preserved() {
        let disasm = "\
00000500 <_ZN8FastLED5beginEv>:
     500:	f7ff fffe 	bl	520 <_ZNSt6vectorIiSaIiEE9push_backERKi>
";
        let edges = parse_disasm(disasm);
        assert_eq!(
            edges.get("_ZN8FastLED5beginEv"),
            Some(&vec!["_ZNSt6vectorIiSaIiEE9push_backERKi".to_string()])
        );
    }

    /// `invert()` produces a callee → callers map; round-tripping a
    /// known small graph proves the inversion is byte-identical to
    /// the original edge set.
    #[test]
    fn invert_is_bijective() {
        let mut forward: BTreeMap<String, Vec<String>> = BTreeMap::new();
        forward.insert("a".to_string(), vec!["b".to_string(), "c".to_string()]);
        forward.insert("b".to_string(), vec!["c".to_string()]);
        let inverse = invert(&forward);
        assert_eq!(inverse.get("b"), Some(&vec!["a".to_string()]));
        assert_eq!(
            inverse.get("c"),
            Some(&vec!["a".to_string(), "b".to_string()])
        );
        // Round-trip back equals the original.
        let round_trip = invert(&inverse);
        assert_eq!(round_trip, forward);
    }

    /// Empty input ⇒ empty output, no panic.
    #[test]
    fn empty_input_yields_empty_map() {
        assert!(parse_disasm("").is_empty());
        assert!(parse_disasm("\n\n\nrandom non-disasm text\n").is_empty());
    }
}
