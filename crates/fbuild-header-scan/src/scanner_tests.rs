//! Tests for [`super`].
//!
//! Split out to keep the implementation file under the workspace 1000-LOC
//! limit; `compiler_tests.rs` is the same pattern.

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
    let known: HashSet<String> = ["FL_IS_SAMD21".to_string(), "FL_IS_SAMD51".to_string()].into();
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
