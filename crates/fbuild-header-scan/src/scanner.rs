//! Line-oriented C/C++ `#include` scanner.
//!
//! Tokenizes source byte-by-byte while tracking whether we are inside a line
//! comment, block comment, string literal, raw string literal, or character
//! literal. `#include` directives are recognized only in normal code state.
//! [`scan`] preserves the legacy behavior and scans every conditional branch;
//! [`scan_active`] evaluates active branches for LDF selection.
//! Both branches of `#if` / `#ifdef` are scanned (we do not evaluate
//! preprocessor conditionals — false positives are acceptable, false negatives
//! are not) when using `scan`.

use std::collections::{HashMap, HashSet};

/// Whether an include used `<...>` (system / search-path) or `"..."` (quoted /
/// same-directory-first).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IncludeKind {
    Quoted,
    Angled,
}

/// Position of an `#include` directive within the source. Lines and columns
/// are 1-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub line: u32,
    pub col: u32,
}

/// One `#include` directive extracted from source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncludeRef {
    pub path: String,
    pub kind: IncludeKind,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Code,
    LineComment,
    BlockComment,
    /// Inside `"..."` — `\` escapes the next byte.
    StringLit,
    /// Inside `'...'` — `\` escapes the next byte.
    CharLit,
    /// Inside `R"DELIM(...)DELIM"` — terminated only by `)DELIM"`.
    RawString,
}

/// Extract every `#include` directive from `src`. Pure function; no I/O.
pub fn scan(src: &str) -> Vec<IncludeRef> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut state = State::Code;
    let mut raw_delim: Vec<u8> = Vec::new();
    let mut i = 0usize;
    let mut line: u32 = 1;
    let mut line_start: usize = 0;
    let mut at_line_start_in_code = true;

    while i < bytes.len() {
        let b = bytes[i];

        if b == b'\n' {
            if state == State::LineComment {
                state = State::Code;
            }
            line += 1;
            line_start = i + 1;
            at_line_start_in_code = state == State::Code;
            i += 1;
            continue;
        }

        match state {
            State::LineComment => {
                i += 1;
            }
            State::BlockComment => {
                if b == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                    state = State::Code;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            State::StringLit => {
                if b == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                } else if b == b'"' {
                    state = State::Code;
                    i += 1;
                } else {
                    i += 1;
                }
            }
            State::CharLit => {
                if b == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                } else if b == b'\'' {
                    state = State::Code;
                    i += 1;
                } else {
                    i += 1;
                }
            }
            State::RawString => {
                if b == b')' {
                    let close_len = raw_delim.len() + 2;
                    if i + close_len <= bytes.len()
                        && bytes[i + 1..i + 1 + raw_delim.len()] == raw_delim[..]
                        && bytes[i + close_len - 1] == b'"'
                    {
                        state = State::Code;
                        raw_delim.clear();
                        i += close_len;
                        continue;
                    }
                }
                i += 1;
            }
            State::Code => {
                if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                    state = State::LineComment;
                    i += 2;
                    at_line_start_in_code = false;
                    continue;
                }
                if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
                    state = State::BlockComment;
                    i += 2;
                    at_line_start_in_code = false;
                    continue;
                }
                let prev_is_ident_continuation =
                    i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
                if (b == b'R' || b == b'L' || b == b'u' || b == b'U')
                    && !prev_is_ident_continuation
                    && is_raw_string_open(bytes, i)
                {
                    let open_quote = bytes[i..]
                        .iter()
                        .position(|&c| c == b'"')
                        .expect("fbuild-header-scan: is_raw_string_open guarantees '\"' ahead")
                        + i;
                    let paren = bytes[open_quote + 1..]
                        .iter()
                        .position(|&c| c == b'(')
                        .expect("fbuild-header-scan: is_raw_string_open guarantees '(' after the opening quote")
                        + open_quote
                        + 1;
                    raw_delim.clear();
                    raw_delim.extend_from_slice(&bytes[open_quote + 1..paren]);
                    state = State::RawString;
                    i = paren + 1;
                    at_line_start_in_code = false;
                    continue;
                }
                if b == b'"' {
                    state = State::StringLit;
                    i += 1;
                    at_line_start_in_code = false;
                    continue;
                }
                if b == b'\'' {
                    state = State::CharLit;
                    i += 1;
                    at_line_start_in_code = false;
                    continue;
                }
                if b == b'#' && at_line_start_in_code {
                    if let Some((inc, consumed)) = try_parse_include(bytes, i, line, line_start) {
                        out.push(inc);
                        i += consumed;
                        at_line_start_in_code = false;
                        continue;
                    }
                }
                if !is_horizontal_ws(b) {
                    at_line_start_in_code = false;
                }
                i += 1;
            }
        }
    }

    out
}

/// Extract includes reachable through active preprocessor branches.
///
/// `defines` represents the compiler command line. Macros introduced by an
/// active `#define` in the same file apply to subsequent lines, matching the
/// part of preprocessing relevant to library discovery.
pub fn scan_active(src: &str, defines: &HashMap<String, String>) -> Vec<IncludeRef> {
    scan_active_with_known(src, defines, &HashSet::new())
}

/// [`scan_active`] told which macro names the wider corpus defines.
///
/// `defined_somewhere` is the union of every `#define`d name reachable from
/// the seeds. A guard on a name in that set cannot be decided from the
/// compiler command line alone, so every arm is scanned; a guard on a name
/// nobody defines is honestly false and stays pruned (FastLED/fbuild#1371).
pub fn scan_active_with_known(
    src: &str,
    defines: &HashMap<String, String>,
    defined_somewhere: &HashSet<String>,
) -> Vec<IncludeRef> {
    let mut macros = defines.clone();
    scan(&active_source(src, &mut macros, defined_somewhere))
}

/// Every macro name this source `#define`s, in any branch.
///
/// Deliberately textual: the point is to know what the corpus *could* define,
/// so conditionals must not filter it.
pub fn defined_macro_names(src: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in src.lines() {
        let Some(directive) = line.trim_start().strip_prefix('#').map(str::trim_start) else {
            continue;
        };
        let (name, rest) = split_directive(directive);
        if name != "define" {
            continue;
        }
        let (macro_name, _) = split_directive(rest);
        let macro_name = macro_name.split('(').next().unwrap_or("");
        if !macro_name.is_empty() {
            names.push(macro_name.to_string());
        }
    }
    names
}

/// Return `defines` plus macros declared in active branches of `src`.
///
/// The LDF uses this for sketch translation units so a sketch-local feature
/// define remains visible when its included headers are scanned.
pub fn active_defines(src: &str, defines: &HashMap<String, String>) -> HashMap<String, String> {
    let mut macros = defines.clone();
    let _ = active_source(src, &mut macros, &HashSet::new());
    macros
}

#[derive(Clone, Copy)]
struct Conditional {
    /// Whether lines were being kept when this conditional opened.
    parent_scan: bool,
    /// Whether `#define`s were being applied when this conditional opened.
    parent_define: bool,
    /// A branch of this group was decidably taken, so later `#elif`/`#else`
    /// arms are dead. Never set for an undecidable group.
    branch_taken: bool,
    /// The group's condition could not be decided, so every arm is scanned.
    unknown: bool,
}

/// Apply one branch decision to the scan/define state.
///
/// The two states are deliberately separate. Scanning is generous — the
/// scanner's contract is that false positives are acceptable and false
/// negatives are not — while `#define` application stays strict, because a
/// macro picked up from a branch that may not be compiled would go on to
/// decide *other* conditions wrongly.
fn apply_decision(decision: Decision, parent_scan: bool, parent_define: bool) -> (bool, bool) {
    match decision {
        Decision::True => (parent_scan, parent_define),
        // The LDF `#if 0` hint idiom: never compiled, so an include here is a
        // dependency declaration. Scanned, but its defines are not real.
        Decision::LiteralFalse => (parent_scan, false),
        Decision::False => (false, false),
        Decision::Unknown => (parent_scan, false),
    }
}

fn active_source(
    src: &str,
    macros: &mut HashMap<String, String>,
    defined_somewhere: &HashSet<String>,
) -> String {
    let mut stack: Vec<Conditional> = Vec::new();
    // `scan` keeps lines for the include scan; `active` gates `#define`.
    let mut scan = true;
    let mut active = true;
    let mut output = String::with_capacity(src.len());
    for line in src.split_inclusive('\n') {
        let directive = line.trim_start().strip_prefix('#').map(str::trim_start);
        let mut keep = scan;
        if let Some(directive) = directive {
            let (name, rest) = split_directive(directive);
            match name {
                "if" | "ifdef" | "ifndef" => {
                    let decision = match name {
                        "if" => eval_decision(rest, macros, defined_somewhere),
                        // A bare `#ifdef X` is undecidable for the same reason
                        // `defined(X)` is: the macro set is the command line,
                        // not the preprocessor's running state.
                        "ifdef" => {
                            decide_defined(macros, defined_somewhere, first_token(rest), false)
                        }
                        _ => decide_defined(macros, defined_somewhere, first_token(rest), true),
                    };
                    let (next_scan, next_active) = if scan {
                        apply_decision(decision, scan, active)
                    } else {
                        (false, false)
                    };
                    stack.push(Conditional {
                        parent_scan: scan,
                        parent_define: active,
                        branch_taken: scan && decision == Decision::True,
                        unknown: scan && decision == Decision::Unknown,
                    });
                    scan = next_scan;
                    active = next_active;
                    keep = false;
                }
                "elif" => {
                    if let Some(current) = stack.last_mut() {
                        if current.unknown {
                            // Undecidable group: every arm is scanned, none
                            // contributes defines.
                            scan = current.parent_scan;
                            active = false;
                        } else if current.branch_taken {
                            scan = false;
                            active = false;
                        } else {
                            let decision = eval_decision(rest, macros, defined_somewhere);
                            let (next_scan, next_active) = apply_decision(
                                decision,
                                current.parent_scan,
                                current.parent_define,
                            );
                            scan = next_scan;
                            active = next_active;
                            current.branch_taken |= decision == Decision::True;
                            current.unknown |= decision == Decision::Unknown;
                        }
                    }
                    keep = false;
                }
                "else" => {
                    if let Some(current) = stack.last_mut() {
                        if current.unknown {
                            scan = current.parent_scan;
                            active = false;
                        } else {
                            scan = current.parent_scan && !current.branch_taken;
                            active = current.parent_define && !current.branch_taken;
                            current.branch_taken = true;
                        }
                    }
                    keep = false;
                }
                "endif" => {
                    if let Some(current) = stack.pop() {
                        scan = current.parent_scan;
                        active = current.parent_define;
                    }
                    keep = false;
                }
                "define" if active => {
                    let (name, value) = split_directive(rest);
                    if !name.is_empty() && !name.contains('(') {
                        macros.insert(name.to_string(), first_token(value).to_string());
                    }
                    keep = false;
                }
                "undef" if active => {
                    macros.remove(first_token(rest));
                    keep = false;
                }
                // A `#define`/`#undef` inside a branch that is only being
                // scanned speculatively must not reach the macro set. The
                // directive line itself is never include-bearing either.
                "define" | "undef" => {
                    keep = false;
                }
                _ => {}
            }
        }
        if keep {
            output.push_str(line);
        } else if line.ends_with('\n') {
            output.push('\n');
        }
    }
    output
}

/// Decide an `#ifdef` / `#ifndef` against the available macro set.
///
/// Present means decidable. Absent means *unknown*, not false — see
/// [`Decision`].
fn decide_defined(
    macros: &HashMap<String, String>,
    defined_somewhere: &HashSet<String>,
    name: &str,
    negated: bool,
) -> Decision {
    if name.is_empty() {
        return Decision::LiteralFalse;
    }
    if macros.contains_key(name) {
        if negated {
            Decision::False
        } else {
            Decision::True
        }
    } else if defined_somewhere.contains(name) {
        Decision::Unknown
    } else if negated {
        Decision::True
    } else {
        Decision::False
    }
}

fn split_directive(input: &str) -> (&str, &str) {
    let trimmed = input.trim_start();
    let end = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
    (&trimmed[..end], trimmed[end..].trim_start())
}

fn first_token(input: &str) -> &str {
    input
        .trim_start()
        .split(|c: char| c.is_whitespace() || matches!(c, '/' | '*'))
        .next()
        .unwrap_or("")
}

/// What a preprocessor condition evaluates to, given an incomplete macro set.
///
/// The third state is the point. `scan_active` is handed the *compiler
/// command line* only — macros a header defines are not threaded through the
/// walk, because the walker visits each file once, in BFS order, with a
/// shared cache, and that is not preprocessor order. So a guard like
/// `#if defined(FL_IS_SAMD21)` is not false; it is *unknown*, and the
/// difference matters: FastLED derives `FL_IS_SAMD21` several headers deep
/// from `-D__SAMD21G18A__`, and treating it as false made an include that is
/// genuinely compiled invisible to library selection
/// (FastLED/fbuild#1371).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Decision {
    True,
    /// Decidably false from the macros actually available.
    False,
    /// False, and reached without consulting a single macro — `#if 0`.
    ///
    /// Distinguished from [`Decision::False`] because an include inside a
    /// literal-false block cannot be there to be compiled. It is a dependency
    /// declaration: the PlatformIO LDF `#if 0` hint idiom.
    LiteralFalse,
    /// Referenced a macro that is not in the available set, so the branch
    /// cannot be decided.
    Unknown,
}

fn eval_decision(
    input: &str,
    macros: &HashMap<String, String>,
    defined_somewhere: &HashSet<String>,
) -> Decision {
    let mut parser = ConditionParser {
        input: input.as_bytes(),
        index: 0,
        macros,
        defined_somewhere,
        saw_unknown_macro: false,
        saw_any_macro: false,
    };
    let value = parser.parse_or();
    if parser.saw_unknown_macro {
        Decision::Unknown
    } else if value != 0 {
        Decision::True
    } else if parser.saw_any_macro {
        Decision::False
    } else {
        Decision::LiteralFalse
    }
}

struct ConditionParser<'a> {
    input: &'a [u8],
    index: usize,
    macros: &'a HashMap<String, String>,
    /// Every macro name `#define`d anywhere in the reachable source corpus.
    ///
    /// This is what separates "the project never defines this" from "the
    /// project defines this somewhere the walk could not thread to us". Only
    /// the second is undecidable; the first is honestly false, and treating
    /// it as unknown would select libraries behind branches that genuinely
    /// never compile.
    defined_somewhere: &'a HashSet<String>,
    /// Set when the expression consulted a macro that is not available, which
    /// makes the whole condition undecidable rather than false.
    saw_unknown_macro: bool,
    /// Set when the expression consulted any macro at all, which separates
    /// `#if 0` from a guard that happened to evaluate to zero.
    saw_any_macro: bool,
}

impl<'a> ConditionParser<'a> {
    fn parse_or(&mut self) -> i64 {
        let mut value = self.parse_and();
        while self.consume(b"||") {
            let rhs = self.parse_and();
            value = i64::from(value != 0 || rhs != 0);
        }
        value
    }

    fn parse_and(&mut self) -> i64 {
        let mut value = self.parse_equality();
        while self.consume(b"&&") {
            let rhs = self.parse_equality();
            value = i64::from(value != 0 && rhs != 0);
        }
        value
    }

    fn parse_equality(&mut self) -> i64 {
        let mut value = self.parse_comparison();
        loop {
            if self.consume(b"==") {
                value = i64::from(value == self.parse_comparison());
            } else if self.consume(b"!=") {
                value = i64::from(value != self.parse_comparison());
            } else {
                return value;
            }
        }
    }

    fn parse_comparison(&mut self) -> i64 {
        let mut value = self.parse_unary();
        loop {
            if self.consume(b">=") {
                value = i64::from(value >= self.parse_unary());
            } else if self.consume(b"<=") {
                value = i64::from(value <= self.parse_unary());
            } else if self.consume(b">") {
                value = i64::from(value > self.parse_unary());
            } else if self.consume(b"<") {
                value = i64::from(value < self.parse_unary());
            } else {
                return value;
            }
        }
    }

    fn parse_unary(&mut self) -> i64 {
        if self.consume(b"!") {
            return i64::from(self.parse_unary() == 0);
        }
        if self.consume(b"(") {
            let value = self.parse_or();
            self.consume(b")");
            return value;
        }
        let token = self.token();
        if token == "defined" {
            self.consume(b"(");
            let name = self.token();
            self.consume(b")");
            self.saw_any_macro = true;
            if !self.macros.contains_key(name) && self.defined_somewhere.contains(name) {
                self.saw_unknown_macro = true;
            }
            return i64::from(self.macros.contains_key(name));
        }
        if let Some(value) = parse_number(token) {
            return value;
        }
        if token.is_empty() {
            return 0;
        }
        self.saw_any_macro = true;
        match self.macros.get(token).and_then(|value| parse_number(value)) {
            Some(value) => value,
            None => {
                if self.defined_somewhere.contains(token) {
                    self.saw_unknown_macro = true;
                }
                0
            }
        }
    }

    fn consume(&mut self, expected: &[u8]) -> bool {
        self.skip_ws();
        if self.input[self.index..].starts_with(expected) {
            self.index += expected.len();
            true
        } else {
            false
        }
    }

    fn token(&mut self) -> &'a str {
        self.skip_ws();
        let start = self.index;
        while self.index < self.input.len()
            && (self.input[self.index].is_ascii_alphanumeric() || self.input[self.index] == b'_')
        {
            self.index += 1;
        }
        std::str::from_utf8(&self.input[start..self.index]).unwrap_or("")
    }

    fn skip_ws(&mut self) {
        while self.index < self.input.len() && self.input[self.index].is_ascii_whitespace() {
            self.index += 1;
        }
    }
}

fn parse_number(value: &str) -> Option<i64> {
    let value = value.trim_end_matches(['u', 'U', 'l', 'L']);
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        i64::from_str_radix(hex, 16).ok()
    } else {
        value.parse().ok()
    }
}

fn is_horizontal_ws(b: u8) -> bool {
    b == b' ' || b == b'\t' || b == b'\r'
}

/// Recognise `R"`, `LR"`, `uR"`, `UR"`, `u8R"` raw-string openers. Caller has
/// already matched the leading byte at index `i`.
fn is_raw_string_open(bytes: &[u8], i: usize) -> bool {
    let mut j = i;
    if bytes[j] == b'u' && j + 1 < bytes.len() && bytes[j + 1] == b'8' {
        j += 2;
    } else if matches!(bytes[j], b'L' | b'u' | b'U') {
        j += 1;
    }
    if j >= bytes.len() || bytes[j] != b'R' {
        return false;
    }
    j += 1;
    if j >= bytes.len() || bytes[j] != b'"' {
        return false;
    }
    let after_quote = j + 1;
    let mut k = after_quote;
    while k < bytes.len() && bytes[k] != b'(' && bytes[k] != b'\n' && bytes[k] != b'"' {
        k += 1;
    }
    k < bytes.len() && bytes[k] == b'('
}

/// Try to parse a `#include` directive starting at `bytes[hash_pos] = '#'`.
/// Returns `(IncludeRef, bytes_consumed_from_hash_pos)` or `None` if this is
/// some other preprocessor directive.
fn try_parse_include(
    bytes: &[u8],
    hash_pos: usize,
    line: u32,
    line_start: usize,
) -> Option<(IncludeRef, usize)> {
    let mut p = hash_pos + 1;
    while p < bytes.len() && is_horizontal_ws(bytes[p]) {
        p += 1;
    }
    if p + 7 > bytes.len() || &bytes[p..p + 7] != b"include" {
        return None;
    }
    p += 7;
    while p < bytes.len() && is_horizontal_ws(bytes[p]) {
        p += 1;
    }
    if p >= bytes.len() {
        return None;
    }
    let (open, close, kind) = match bytes[p] {
        b'<' => (b'<', b'>', IncludeKind::Angled),
        b'"' => (b'"', b'"', IncludeKind::Quoted),
        _ => return None,
    };
    let _ = open;
    p += 1;
    let path_start = p;
    while p < bytes.len() && bytes[p] != close && bytes[p] != b'\n' {
        p += 1;
    }
    if p >= bytes.len() || bytes[p] != close {
        return None;
    }
    let path = match std::str::from_utf8(&bytes[path_start..p]) {
        Ok(s) => s.to_string(),
        Err(_) => return None,
    };
    p += 1;
    let col = (hash_pos - line_start + 1) as u32;
    Some((
        IncludeRef {
            path,
            kind,
            span: Span { line, col },
        },
        p - hash_pos,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first(refs: &[IncludeRef]) -> &IncludeRef {
        refs.first().expect("expected at least one include ref")
    }

    #[test]
    fn s01_angled() {
        let refs = scan("#include <stdio.h>");
        assert_eq!(refs.len(), 1);
        assert_eq!(first(&refs).path, "stdio.h");
        assert_eq!(first(&refs).kind, IncludeKind::Angled);
    }

    #[test]
    fn s02_quoted() {
        let refs = scan("#include \"foo.h\"");
        assert_eq!(refs.len(), 1);
        assert_eq!(first(&refs).path, "foo.h");
        assert_eq!(first(&refs).kind, IncludeKind::Quoted);
    }

    #[test]
    fn s03_leading_ws() {
        let refs = scan("  #include <a.h>");
        assert_eq!(refs.len(), 1);
        assert_eq!(first(&refs).path, "a.h");
    }

    #[test]
    fn s04_ws_after_hash() {
        let refs = scan("#  include <a.h>");
        assert_eq!(refs.len(), 1);
        assert_eq!(first(&refs).path, "a.h");
    }

    #[test]
    fn s05_path_with_slashes() {
        let refs = scan("#include <a/b/c.h>");
        assert_eq!(refs.len(), 1);
        assert_eq!(first(&refs).path, "a/b/c.h");
    }

    #[test]
    fn s06_trailing_comment_ignored() {
        let refs = scan("#include   <a.h>  // trailing\n");
        assert_eq!(refs.len(), 1);
        assert_eq!(first(&refs).path, "a.h");
    }

    #[test]
    fn s07_garbage_after_first_include_does_not_crash() {
        let refs = scan("#include \"a.h\" \"b.h\"\n");
        assert_eq!(refs.len(), 1);
        assert_eq!(first(&refs).path, "a.h");
    }

    #[test]
    fn s10_line_comment_blocks_include() {
        let refs = scan("// #include <evil.h>\n");
        assert!(refs.is_empty(), "got {refs:?}");
    }

    #[test]
    fn s11_block_comment_blocks_include() {
        let refs = scan("/* #include <evil.h> */\n");
        assert!(refs.is_empty(), "got {refs:?}");
    }

    #[test]
    fn s12_multiline_block_comment_blocks_include() {
        let refs = scan("/*\n#include <evil.h>\n*/\n");
        assert!(refs.is_empty(), "got {refs:?}");
    }

    #[test]
    fn s13_string_literal_blocks_include() {
        let refs = scan("const char* s = \"#include <evil.h>\";\n");
        assert!(refs.is_empty(), "got {refs:?}");
    }

    #[test]
    fn s14_escaped_quotes_in_string_blocks_include() {
        let refs = scan("const char* s = \"\\\"#include <evil.h>\\\"\";\n");
        assert!(refs.is_empty(), "got {refs:?}");
    }

    #[test]
    fn s15_raw_string_blocks_include() {
        let refs = scan("const char* s = R\"(#include <evil.h>)\";\n");
        assert!(refs.is_empty(), "got {refs:?}");
    }

    #[test]
    fn s15_raw_string_with_delim_blocks_include() {
        let refs = scan("const char* s = R\"DELIM(#include <evil.h>)DELIM\";\n");
        assert!(refs.is_empty(), "got {refs:?}");
    }

    #[test]
    fn s16_char_literal_does_not_swallow() {
        let refs = scan("char c = '#';\n#include <a.h>\n");
        assert_eq!(refs.len(), 1);
        assert_eq!(first(&refs).path, "a.h");
    }

    #[test]
    fn s17_line_comment_then_include() {
        let refs = scan("//#include <a.h>\n#include <b.h>\n");
        assert_eq!(refs.len(), 1);
        assert_eq!(first(&refs).path, "b.h");
    }

    #[test]
    fn s20_span_line_after_blank_lines() {
        let refs = scan("\n\n#include <a.h>");
        assert_eq!(first(&refs).span.line, 3);
        assert_eq!(first(&refs).span.col, 1);
    }

    #[test]
    fn s21_span_col_with_indent() {
        let refs = scan("  #include <a.h>");
        assert_eq!(first(&refs).span.line, 1);
        assert_eq!(first(&refs).span.col, 3);
    }

    #[test]
    fn s30_if_zero_branch_still_scanned() {
        let refs = scan("#if 0\n#include <a.h>\n#endif\n");
        assert_eq!(refs.len(), 1);
        assert_eq!(first(&refs).path, "a.h");
    }

    #[test]
    fn s31_has_include_branch_still_scanned() {
        let refs = scan("#ifdef __has_include\n#include <a.h>\n#endif\n");
        assert_eq!(refs.len(), 1);
    }

    #[test]
    fn s32_both_branches_scanned() {
        let refs = scan("#if defined(X)\n#include <a.h>\n#else\n#include <b.h>\n#endif\n");
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].path, "a.h");
        assert_eq!(refs[1].path, "b.h");
    }

    #[test]
    fn ignores_other_directives() {
        let refs = scan("#define FOO 1\n#pragma once\n");
        assert!(refs.is_empty());
    }

    #[test]
    fn handles_crlf_line_endings() {
        let refs = scan("#include <a.h>\r\n#include <b.h>\r\n");
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].span.line, 1);
        assert_eq!(refs[1].span.line, 2);
    }

    #[test]
    fn does_not_panic_on_unterminated_block_comment() {
        let _ = scan("/* unterminated");
    }

    #[test]
    fn does_not_panic_on_unterminated_string() {
        let _ = scan("const char* s = \"unterminated");
    }

    #[test]
    fn does_not_panic_on_unterminated_raw_string() {
        let _ = scan("const char* s = R\"DELIM(unterminated");
    }

    #[test]
    fn identifier_ending_in_r_does_not_start_raw_string() {
        // `FooR` ends in `R` but is an identifier — the next `R"(` must NOT
        // be treated as the opener of a raw string. If it were, the scanner
        // would consume into RawString state and silently swallow the
        // `#include` on the following line — a false negative the module
        // contract forbids.
        let refs = scan("auto FooR = 0;\n#include <a.h>\n");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].path, "a.h");
    }

    #[test]
    fn identifier_ending_in_lr_does_not_start_wide_raw_string() {
        // `FooL` precedes `R"(` — the `L` is part of the identifier, not the
        // wide-string prefix. Must NOT enter RawString state.
        let refs = scan("auto FooL = 0;\n#include <a.h>\n");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].path, "a.h");
    }

    #[test]
    fn identifier_ending_in_lower_u_r_does_not_start_raw_string() {
        let refs = scan("auto Foou = 0;\n#include <a.h>\n");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].path, "a.h");
    }

    #[test]
    fn identifier_ending_in_upper_u_r_does_not_start_raw_string() {
        let refs = scan("auto FooU = 0;\n#include <a.h>\n");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].path, "a.h");
    }

    #[test]
    fn underscore_before_raw_prefix_blocks_detection() {
        // `_R"(...)"` is identifier-continuation; must not start a raw
        // string. Critical for code that uses `_R` as a translation macro
        // name (common in i18n shims).
        let refs = scan("foo_R = 0;\n#include <a.h>\n");
        assert_eq!(refs.len(), 1);
    }

    #[test]
    fn digit_before_raw_prefix_blocks_detection() {
        // Numbers can appear in identifiers; `foo1R` must not start a raw
        // string.
        let refs = scan("foo1R = 0;\n#include <a.h>\n");
        assert_eq!(refs.len(), 1);
    }

    #[test]
    fn whitespace_before_raw_prefix_starts_raw_string() {
        // Positive control — make sure we didn't break legitimate raw
        // strings preceded by whitespace.
        let refs = scan("auto x = R\"(#include <evil.h>)\";\n#include <a.h>\n");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].path, "a.h");
    }

    #[test]
    fn start_of_file_raw_string_still_detected() {
        // Boundary case: `R"(...)"` at byte 0 has no previous byte;
        // `i > 0` clause must short-circuit and allow detection.
        let refs = scan("R\"(#include <evil.h>)\"\n#include <a.h>\n");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].path, "a.h");
    }

    #[test]
    fn punctuation_before_raw_prefix_starts_raw_string() {
        // `=R"(...)"` — `=` is non-identifier; must enter raw-string state
        // and swallow the embedded `#include`.
        let refs = scan("auto x =R\"(#include <evil.h>)\";\n#include <a.h>\n");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].path, "a.h");
    }

    #[test]
    fn paren_before_raw_prefix_starts_raw_string() {
        // `(R"(...)"` — `(` is non-identifier.
        let refs = scan("foo(R\"(#include <evil.h>)\");\n#include <a.h>\n");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].path, "a.h");
    }

    #[test]
    fn many_includes_in_one_file() {
        // Adversary: pile of includes interspersed with comments and
        // strings. Confirm count + order are stable.
        let src = "// header\n\
                   #include <a.h>\n\
                   const char* s = \"#include <not_real.h>\";\n\
                   #include \"b.h\"\n\
                   /* block\n\
                      #include <also_not_real.h>\n\
                   */\n\
                   #include <c.h>\n";
        let refs = scan(src);
        assert_eq!(refs.len(), 3);
        assert_eq!(refs[0].path, "a.h");
        assert_eq!(refs[1].path, "b.h");
        assert_eq!(refs[2].path, "c.h");
    }

    #[test]
    fn empty_input_returns_empty() {
        assert!(scan("").is_empty());
    }

    #[test]
    fn lone_hash_does_not_panic() {
        let _ = scan("#");
    }

    #[test]
    fn hash_then_eof_does_not_panic() {
        let _ = scan("#include");
    }

    #[test]
    fn null_bytes_do_not_panic() {
        // Adversary: embedded NUL inside source. Real toolchains reject
        // these but the scanner must not crash.
        let _ = scan("foo\0bar\n#include <a.h>\n");
    }

    #[test]
    fn very_long_line_does_not_panic() {
        // 64 KB single line.
        let mut s = String::from("// ");
        s.push_str(&"x".repeat(64 * 1024));
        s.push('\n');
        s.push_str("#include <a.h>\n");
        let refs = scan(&s);
        assert_eq!(refs.len(), 1);
    }

    #[test]
    fn deeply_nested_block_comments_do_not_panic() {
        // C/C++ block comments don't nest, but we still shouldn't choke on
        // pathological input.
        let s = "/* /* /* */\n#include <a.h>\n";
        let refs = scan(s);
        // After the first `*/`, we're back in code state, so the include
        // must be picked up.
        assert_eq!(refs.len(), 1);
    }

    #[test]
    /// `#if 0` is a dependency declaration, not dead code.
    ///
    /// This test previously asserted that `Audio.h` was ignored. It is not,
    /// and deliberately so: an include that can never be compiled is only
    /// there to be *seen* — the PlatformIO LDF hint idiom, which FastLED uses
    /// in `platforms/*/ldf_headers.h` to declare dependencies its conditional
    /// includes would otherwise hide (FastLED/fbuild#1371). PlatformIO's own
    /// `chain` mode honors it by not evaluating conditionals at all.
    ///
    /// The `#else` arm is scanned too, because that is the arm which actually
    /// compiles. Both are dependencies.
    fn literal_false_branches_are_scanned_as_ldf_hints() {
        let refs = scan_active(
            "#if 0\n#include <Audio.h>\n#else\n#include <SPI.h>\n#endif\n",
            &HashMap::new(),
        );
        let paths: Vec<&str> = refs.iter().map(|r| r.path.as_str()).collect();
        assert!(
            paths.contains(&"Audio.h"),
            "the #if 0 hint must be seen: {paths:?}"
        );
        assert!(
            paths.contains(&"SPI.h"),
            "the compiled arm must be seen: {paths:?}"
        );
    }

    /// A branch that is decidably false from the *known* macros stays pruned.
    ///
    /// This is what keeps the change from collapsing into a plain textual
    /// scan: when the command line actually settles a guard, it is settled.
    #[test]
    fn decidably_false_branches_are_still_pruned() {
        let mut defines = HashMap::new();
        defines.insert("USE_AUDIO".to_string(), "0".to_string());
        let refs = scan_active(
            "#if USE_AUDIO\n#include <Audio.h>\n#else\n#include <SPI.h>\n#endif\n",
            &defines,
        );
        let paths: Vec<&str> = refs.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["SPI.h"],
            "a known-false guard must still prune: {paths:?}"
        );
    }

    /// The FastLED/fbuild#1371 case: a guard on a macro the scan cannot see.
    ///
    /// `FL_IS_SAMD21` is derived several headers deep from `-D__SAMD21G18A__`,
    /// and header-defined macros are not threaded through the walk. Treating
    /// that as *false* made an include that is genuinely compiled invisible to
    /// library selection; treating it as *unknown* finds it.
    #[test]
    fn guards_on_unknown_macros_scan_every_arm() {
        // The corpus defines these somewhere (FastLED's `is_platform.h`), so
        // the guard is undecidable rather than false.
        let known: HashSet<String> =
            ["FL_IS_SAMD21".to_string(), "FL_IS_SAMD51".to_string()].into();
        let refs = scan_active_with_known(
            "#if defined(FL_IS_SAMD21) || defined(FL_IS_SAMD51)\n#include <SPI.h>\n#endif\n",
            &HashMap::new(),
            &known,
        );
        let paths: Vec<&str> = refs.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["SPI.h"],
            "an undecidable guard must not hide its include"
        );
    }

    /// `#ifdef` on an unseen macro is undecidable for the same reason.
    #[test]
    fn ifdef_on_an_unknown_macro_is_undecidable() {
        let known: HashSet<String> = ["FL_IS_ARM".to_string()].into();
        let refs = scan_active_with_known(
            "#ifdef FL_IS_ARM\n#include <arm.h>\n#else\n#include <other.h>\n#endif\n",
            &HashMap::new(),
            &known,
        );
        let paths: Vec<&str> = refs.iter().map(|r| r.path.as_str()).collect();
        assert!(paths.contains(&"arm.h"), "{paths:?}");
        assert!(paths.contains(&"other.h"), "{paths:?}");
    }

    /// Defines from a speculatively-scanned branch must not settle later
    /// guards — that would let an arm which may never compile prune a real one.
    #[test]
    fn defines_inside_undecidable_branches_do_not_leak() {
        let known: HashSet<String> = ["UNKNOWN_MACRO".to_string(), "PICKED".to_string()].into();
        let refs = scan_active_with_known(
            "#ifdef UNKNOWN_MACRO\n#define PICKED 1\n#endif\n#if PICKED\n#include <picked.h>\n#else\n#include <other.h>\n#endif\n",
            &HashMap::new(),
            &known,
        );
        let paths: Vec<&str> = refs.iter().map(|r| r.path.as_str()).collect();
        // `PICKED` never became known, so the second guard is undecidable too
        // and both arms are scanned — rather than `PICKED` being trusted.
        assert!(paths.contains(&"picked.h"), "{paths:?}");
        assert!(paths.contains(&"other.h"), "{paths:?}");
    }

    #[test]
    fn active_scan_uses_compiler_and_local_defines() {
        let mut defines = HashMap::new();
        defines.insert("ARDUINO".to_string(), "10819".to_string());
        let refs = scan_active(
            "#if defined(ARDUINO) && ARDUINO >= 100\n#define USE_SPI 1\n#endif\n#ifdef USE_SPI\n#include <SPI.h>\n#endif\n",
            &defines,
        );
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].path, "SPI.h");
    }
}
