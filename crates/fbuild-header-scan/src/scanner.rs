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

use std::collections::HashMap;

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
    let mut macros = defines.clone();
    scan(&active_source(src, &mut macros))
}

/// Return `defines` plus macros declared in active branches of `src`.
///
/// The LDF uses this for sketch translation units so a sketch-local feature
/// define remains visible when its included headers are scanned.
pub fn active_defines(src: &str, defines: &HashMap<String, String>) -> HashMap<String, String> {
    let mut macros = defines.clone();
    let _ = active_source(src, &mut macros);
    macros
}

#[derive(Clone, Copy)]
struct Conditional {
    parent_active: bool,
    branch_taken: bool,
}

fn active_source(src: &str, macros: &mut HashMap<String, String>) -> String {
    let mut stack = Vec::new();
    let mut active = true;
    let mut output = String::with_capacity(src.len());
    for line in src.split_inclusive('\n') {
        let directive = line.trim_start().strip_prefix('#').map(str::trim_start);
        let mut keep = active;
        if let Some(directive) = directive {
            let (name, rest) = split_directive(directive);
            match name {
                "if" => {
                    let current = active && eval_condition(rest, macros);
                    stack.push(Conditional {
                        parent_active: active,
                        branch_taken: current,
                    });
                    active = current;
                    keep = false;
                }
                "ifdef" => {
                    let current = active && macros.contains_key(first_token(rest));
                    stack.push(Conditional {
                        parent_active: active,
                        branch_taken: current,
                    });
                    active = current;
                    keep = false;
                }
                "ifndef" => {
                    let current = active && !macros.contains_key(first_token(rest));
                    stack.push(Conditional {
                        parent_active: active,
                        branch_taken: current,
                    });
                    active = current;
                    keep = false;
                }
                "elif" => {
                    if let Some(current) = stack.last_mut() {
                        active = current.parent_active
                            && !current.branch_taken
                            && eval_condition(rest, macros);
                        current.branch_taken |= active;
                    }
                    keep = false;
                }
                "else" => {
                    if let Some(current) = stack.last_mut() {
                        active = current.parent_active && !current.branch_taken;
                        current.branch_taken = true;
                    }
                    keep = false;
                }
                "endif" => {
                    if let Some(current) = stack.pop() {
                        active = current.parent_active;
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

fn eval_condition(input: &str, macros: &HashMap<String, String>) -> bool {
    let mut parser = ConditionParser {
        input: input.as_bytes(),
        index: 0,
        macros,
    };
    parser.parse_or() != 0
}

struct ConditionParser<'a> {
    input: &'a [u8],
    index: usize,
    macros: &'a HashMap<String, String>,
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
            return i64::from(self.macros.contains_key(name));
        }
        if let Some(value) = parse_number(token) {
            return value;
        }
        self.macros
            .get(token)
            .and_then(|value| parse_number(value))
            .unwrap_or(0)
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
    fn active_scan_ignores_disabled_branch() {
        let refs = scan_active(
            "#if 0\n#include <Audio.h>\n#else\n#include <SPI.h>\n#endif\n",
            &HashMap::new(),
        );
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].path, "SPI.h");
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
