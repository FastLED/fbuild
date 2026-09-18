//! FastLED/fbuild#1440: `.ino` include hoisting must never lift an `#include`
//! out of its `#if`. Split from `tests.rs` to keep it under the 1000-LOC gate.

use super::tests::setup_project;
use super::*;
use std::fs;

/// Generated `.ino.cpp` for a single-tab sketch, split at its `#line 1`.
fn generated_prelude_and_body(sketch: &str) -> (String, String) {
    let (_tmp, src_dir, build_dir) = setup_project(&[("sketch.ino", sketch)]);
    let scanner = SourceScanner::new(&src_dir, &build_dir);
    let sources = scanner.scan_sketch_sources().unwrap();
    let content = fs::read_to_string(&sources[0]).unwrap();
    let (prelude, body) = content
        .split_once("#line 1 \"src/sketch.ino\"\n")
        .expect("generated file carries a #line 1 directive");
    (prelude.to_string(), body.to_string())
}

#[test]
fn test_conditional_include_stays_inside_its_if() {
    // The FastLED Sailboat shape that broke every non-Teensy root build.
    let (prelude, body) = generated_prelude_and_body(
        "#include <FastLED.h>\n\n#if defined(FL_IS_TEENSY)\n#include <Audio.h>\n#endif\n#include \"fl/ui/ui.h\"\n\nint helper(int x) { return x; }\nvoid setup() {}\nvoid loop() {}\n",
    );
    let lines: Vec<&str> = prelude.lines().collect();
    let if_pos = lines
        .iter()
        .position(|l| *l == "#if defined(FL_IS_TEENSY)")
        .expect("the #if moves with its include");
    assert_eq!(lines[if_pos + 1], "#include <Audio.h>");
    assert_eq!(lines[if_pos + 2], "#endif");
    // Exactly one Audio.h, and only inside the guard.
    assert_eq!(prelude.matches("#include <Audio.h>").count(), 1);
    assert!(!body.contains("#include <Audio.h>"));
    // Headers after the block still precede the prototypes.
    let ui_pos = prelude.find("#include \"fl/ui/ui.h\"").unwrap();
    let proto_pos = prelude
        .find("// Auto-generated function prototypes")
        .unwrap();
    assert!(ui_pos < proto_pos);
}

#[test]
fn test_define_before_include_keeps_its_order() {
    let (prelude, _body) = generated_prelude_and_body(
        "#define FASTLED_CONFIG_KNOB 1\n#include <FastLED.h>\n\nvoid setup() {}\nvoid loop() {}\n",
    );
    let define_pos = prelude.find("#define FASTLED_CONFIG_KNOB 1").unwrap();
    let include_pos = prelude.find("#include <FastLED.h>").unwrap();
    assert!(
        define_pos < include_pos,
        "a #define that configures a header must still precede it"
    );
}

#[test]
fn test_include_after_first_code_line_stays_in_place() {
    let (prelude, body) = generated_prelude_and_body(
        "#include <FastLED.h>\nint counter = 0;\n#include \"late.h\"\n\nvoid setup() {}\nvoid loop() {}\n",
    );
    assert!(!prelude.contains("late.h"));
    let body_lines: Vec<&str> = body.lines().collect();
    assert_eq!(body_lines[0], "", "the leading include is blanked");
    assert_eq!(body_lines[1], "int counter = 0;");
    assert_eq!(body_lines[2], "#include \"late.h\"", "left on its own line");
}

#[test]
fn test_spaced_include_directive_is_recognised() {
    let (prelude, body) =
        generated_prelude_and_body("#  include <FastLED.h>\nvoid setup() {}\nvoid loop() {}\n");
    assert!(prelude.contains("#  include <FastLED.h>"));
    assert!(!body.contains("include <FastLED.h>"));
}

#[test]
fn test_include_inside_block_comment_is_not_a_directive() {
    let (prelude, body) = generated_prelude_and_body(
        "/*\n#include <Nope.h>\n*/\n#include <FastLED.h>\nvoid setup() {}\nvoid loop() {}\n",
    );
    // The comment moves verbatim, so the commented-out include is still
    // inside a comment -- never a bare directive.
    let comment_open = prelude.find("/*").unwrap();
    let nope = prelude.find("#include <Nope.h>").unwrap();
    let comment_close = prelude.find("*/").unwrap();
    assert!(comment_open < nope && nope < comment_close);
    assert!(!body.contains("Nope.h"));
}

#[test]
fn test_later_tab_hoists_only_unconditional_includes() {
    let (_tmp, src_dir, build_dir) = setup_project(&[
        (
            "main.ino",
            "#include <FastLED.h>\nvoid setup() {}\nvoid loop() {}\n",
        ),
        (
            "tab.ino",
            "#define TAB_ONLY 1\n#include <Wire.h>\n#ifdef ARDUINO_TEENSY41\n#include <Audio.h>\n#endif\nvoid helper() {}\n",
        ),
    ]);
    let scanner = SourceScanner::new(&src_dir, &build_dir);
    let sources = scanner.scan_sketch_sources().unwrap();
    let content = fs::read_to_string(&sources[0]).unwrap();
    let (prelude, rest) = content.split_once("#line 1 \"src/main.ino\"\n").unwrap();
    let (_, tab_body) = rest.split_once("#line 1 \"src/tab.ino\"\n").unwrap();

    assert!(prelude.contains("#include <Wire.h>"));
    // A later tab's #define and guarded include stay in that tab, in order.
    assert!(!prelude.contains("TAB_ONLY"));
    assert!(!prelude.contains("Audio.h"));
    let tab_lines: Vec<&str> = tab_body.lines().collect();
    assert_eq!(tab_lines[0], "#define TAB_ONLY 1");
    assert_eq!(tab_lines[1], "", "the unconditional include is blanked");
    assert_eq!(tab_lines[2], "#ifdef ARDUINO_TEENSY41");
    assert_eq!(tab_lines[3], "#include <Audio.h>");
    assert_eq!(tab_lines[4], "#endif");
}
