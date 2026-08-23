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

/// The name of this file's own include guard, if it has the standard shape.
///
/// A header that opens `#ifndef FOO_H` / `#define FOO_H` defines its own guard
/// macro, so `FOO_H` lands in the corpus-wide name set and the guard would
/// read as *undecidable* — which would switch off `#define` application for
/// the entire body of nearly every header in the project, and cascade into
/// every later guard in the same file. The guard is not really undecidable:
/// on the inclusion that matters it is not yet defined, so the body is taken.
fn self_include_guard(src: &str) -> Option<String> {
    let mut directives = src.lines().filter_map(|line| {
        let directive = line.trim_start().strip_prefix('#')?.trim_start();
        let (name, rest) = split_directive(directive);
        if name.is_empty() {
            None
        } else {
            Some((name, rest))
        }
    });
    let (first_name, first_rest) = directives.next()?;
    if first_name != "ifndef" {
        return None;
    }
    let guard = first_token(first_rest);
    if guard.is_empty() {
        return None;
    }
    let (second_name, second_rest) = directives.next()?;
    if second_name == "define" && first_token(second_rest) == guard {
        Some(guard.to_string())
    } else {
        None
    }
}

fn active_source(
    src: &str,
    macros: &mut HashMap<String, String>,
    defined_somewhere: &HashSet<String>,
) -> String {
    let self_guard = self_include_guard(src);
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
                    // A file's own `#ifndef FOO_H` is taken on the inclusion
                    // that matters, whatever the corpus says about `FOO_H`.
                    let decision = if name == "ifndef"
                        && self_guard.as_deref() == Some(first_token(rest))
                        && !macros.contains_key(first_token(rest))
                    {
                        Decision::True
                    } else {
                        decision
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
#[path = "scanner_tests.rs"]
mod tests;
