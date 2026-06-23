//! Unit tests for the parent `source_scanner` module. Extracted to keep the
//! parent file under the 1000-LOC gate (see ci.yml LOC Gate workflow).

use super::*;
use std::fs;
use tempfile::TempDir;

fn setup_project(src_files: &[(&str, &str)]) -> (TempDir, PathBuf, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let src_dir = tmp.path().join("src");
    let build_dir = tmp.path().join("build");
    fs::create_dir_all(&src_dir).unwrap();
    fs::create_dir_all(&build_dir).unwrap();

    for (name, content) in src_files {
        let path = src_dir.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, content).unwrap();
    }

    (tmp, src_dir, build_dir)
}

#[test]
fn test_scan_empty_directory() {
    let tmp = TempDir::new().unwrap();
    let src_dir = tmp.path().join("src");
    let build_dir = tmp.path().join("build");
    fs::create_dir_all(&src_dir).unwrap();
    fs::create_dir_all(&build_dir).unwrap();

    let scanner = SourceScanner::new(&src_dir, &build_dir);
    let sources = scanner.scan_sketch_sources().unwrap();
    assert!(sources.is_empty());
}

#[test]
fn test_nonexistent_source_directory() {
    let tmp = TempDir::new().unwrap();
    let scanner = SourceScanner::new(&tmp.path().join("nonexistent"), &tmp.path().join("build"));
    let sources = scanner.scan_sketch_sources().unwrap();
    assert!(sources.is_empty());
}

#[test]
fn test_scan_cpp_files() {
    let (_tmp, src_dir, build_dir) = setup_project(&[("main.cpp", "int main() { return 0; }")]);
    let scanner = SourceScanner::new(&src_dir, &build_dir);
    let sources = scanner.scan_sketch_sources().unwrap();
    assert_eq!(sources.len(), 1);
    assert!(sources[0].to_string_lossy().contains("main.cpp"));
}

#[test]
fn test_scan_cxx_files() {
    let (_tmp, src_dir, build_dir) = setup_project(&[("helper.cxx", "void helper() {}")]);
    let scanner = SourceScanner::new(&src_dir, &build_dir);
    let sources = scanner.scan_sketch_sources().unwrap();
    assert_eq!(sources.len(), 1);
    assert!(sources[0].to_string_lossy().contains("helper.cxx"));
}

#[test]
fn test_scan_c_files() {
    let (_tmp, src_dir, build_dir) = setup_project(&[("helper.c", "void helper() {}")]);
    let scanner = SourceScanner::new(&src_dir, &build_dir);
    let sources = scanner.scan_sketch_sources().unwrap();
    assert_eq!(sources.len(), 1);
    assert!(sources[0].to_string_lossy().contains("helper.c"));
}

#[test]
fn test_scan_single_ino_file() {
    let (_tmp, src_dir, build_dir) =
        setup_project(&[("sketch.ino", "void setup() {}\nvoid loop() {}\n")]);
    let scanner = SourceScanner::new(&src_dir, &build_dir);
    let sources = scanner.scan_sketch_sources().unwrap();
    assert_eq!(sources.len(), 1);
    assert!(sources[0].to_string_lossy().contains(".ino.cpp"));

    // Direct sketch scans do not know framework include roots.
    let content = fs::read_to_string(&sources[0]).unwrap();
    assert!(!content.contains("#include <Arduino.h>"));
}

#[test]
fn test_scan_multiple_ino_files() {
    let (_tmp, src_dir, build_dir) = setup_project(&[
        ("a.ino", "void helperA() {}\n"),
        ("b.ino", "void helperB() {}\n"),
    ]);
    let scanner = SourceScanner::new(&src_dir, &build_dir);
    let sources = scanner.scan_sketch_sources().unwrap();
    assert_eq!(sources.len(), 1); // Concatenated into one .cpp
    let content = fs::read_to_string(&sources[0]).unwrap();
    assert!(content.contains("helperA"));
    assert!(content.contains("helperB"));
}

#[test]
fn test_scan_multiple_ino_files_uses_platformio_main_first() {
    let (_tmp, src_dir, build_dir) = setup_project(&[
        ("z_tab.ino", "void zTab() {}\n"),
        ("main.ino", "void setup() {}\nvoid loop() {}\n"),
        ("a_tab.ino", "void aTab() {}\n"),
    ]);
    let scanner = SourceScanner::new(&src_dir, &build_dir);

    let sources = scanner.scan_sketch_sources().unwrap();
    assert_eq!(sources.len(), 1);
    assert!(sources[0].ends_with("main.ino.cpp"));
    let content = fs::read_to_string(&sources[0]).unwrap();

    let main_pos = content.rfind("void setup()").unwrap();
    let a_pos = content.rfind("void aTab()").unwrap();
    let z_pos = content.rfind("void zTab()").unwrap();
    assert!(main_pos < a_pos);
    assert!(a_pos < z_pos);
}

#[test]
fn test_scan_multiple_ino_files_uses_arduino_named_primary_first() {
    let tmp = TempDir::new().unwrap();
    let src_dir = tmp.path().join("Blink");
    let build_dir = tmp.path().join("build");
    fs::create_dir_all(&src_dir).unwrap();
    fs::create_dir_all(&build_dir).unwrap();
    fs::write(src_dir.join("z_tab.ino"), "void zTab() {}\n").unwrap();
    fs::write(
        src_dir.join("Blink.ino"),
        "void setup() {}\nvoid loop() {}\n",
    )
    .unwrap();
    fs::write(src_dir.join("a_tab.ino"), "void aTab() {}\n").unwrap();
    let scanner = SourceScanner::new(&src_dir, &build_dir);

    let sources = scanner.scan_sketch_sources().unwrap();
    assert_eq!(sources.len(), 1);
    assert!(sources[0].ends_with("Blink.ino.cpp"));
    let content = fs::read_to_string(&sources[0]).unwrap();

    let primary_pos = content.rfind("void setup()").unwrap();
    let a_pos = content.rfind("void aTab()").unwrap();
    let z_pos = content.rfind("void zTab()").unwrap();
    assert!(primary_pos < a_pos);
    assert!(a_pos < z_pos);
}

#[test]
fn test_scan_multiple_ino_files_falls_back_to_setup_loop_primary() {
    let (_tmp, src_dir, build_dir) = setup_project(&[
        ("a_tab.ino", "void aTab() {}\n"),
        ("z_entry.ino", "void setup() {}\nvoid loop() {}\n"),
        ("b_tab.ino", "void bTab() {}\n"),
    ]);
    let scanner = SourceScanner::new(&src_dir, &build_dir);

    let sources = scanner.scan_sketch_sources().unwrap();
    assert_eq!(sources.len(), 1);
    assert!(sources[0].ends_with("z_entry.ino.cpp"));
    let content = fs::read_to_string(&sources[0]).unwrap();

    let primary_pos = content.rfind("void setup()").unwrap();
    let a_pos = content.rfind("void aTab()").unwrap();
    let b_pos = content.rfind("void bTab()").unwrap();
    assert!(primary_pos < a_pos);
    assert!(a_pos < b_pos);
}

#[test]
fn test_scan_mixed_sources() {
    let (_tmp, src_dir, build_dir) = setup_project(&[
        ("sketch.ino", "void setup() {}\nvoid loop() {}\n"),
        ("helper.cpp", "void helper() {}"),
        ("util.c", "void util() {}"),
    ]);
    let scanner = SourceScanner::new(&src_dir, &build_dir);
    let sources = scanner.scan_sketch_sources().unwrap();
    assert_eq!(sources.len(), 3); // 1 preprocessed ino + 2 others
}

#[test]
fn test_scan_main_cpp_with_ino_skips_preprocessing_but_keeps_main_cpp() {
    let (_tmp, src_dir, build_dir) = setup_project(&[
        ("main.cpp", "#include \"sketch.ino\"\n"),
        ("sketch.ino", "void setup() {}\nvoid loop() {}\n"),
    ]);
    let scanner = SourceScanner::new(&src_dir, &build_dir);

    let sources = scanner.scan_sketch_sources().unwrap();

    assert_eq!(sources.len(), 1);
    assert!(sources[0].ends_with("main.cpp"));
    assert!(!build_dir.join("sketch.ino.cpp").exists());
}

#[test]
fn test_main_cpp_with_ino_warning_is_yellow_and_clear() {
    let tmp = TempDir::new().unwrap();
    let main_cpp = tmp.path().join("src").join("main.cpp");
    let ino = tmp.path().join("src").join("sketch.ino");
    let mut out = Vec::new();

    write_main_cpp_skips_ino_warning(&mut out, &main_cpp, &[ino]).unwrap();
    let warning = String::from_utf8(out).unwrap();

    assert!(warning.contains("\u{1b}["));
    assert!(warning.contains("warning:"));
    assert!(warning.contains("main.cpp takes precedence"));
    assert!(warning.contains("skipping automatic .ino preprocessing"));
    assert!(warning.contains("sketch.ino"));
}

#[test]
fn test_main_cpp_without_ino_warning_is_silent() {
    let tmp = TempDir::new().unwrap();
    let main_cpp = tmp.path().join("src").join("main.cpp");
    let mut out = Vec::new();

    write_main_cpp_skips_ino_warning(&mut out, &main_cpp, &[]).unwrap();

    assert!(out.is_empty());
}

#[test]
fn test_scan_headers() {
    let (_tmp, src_dir, build_dir) = setup_project(&[
        ("main.cpp", ""),
        ("header.h", "#pragma once"),
        ("header2.hpp", "#pragma once"),
    ]);
    let scanner = SourceScanner::new(&src_dir, &build_dir);
    let headers = scanner.scan_headers(&src_dir);
    assert_eq!(headers.len(), 2);
}

#[test]
fn test_scan_subdirectories() {
    let (_tmp, src_dir, build_dir) = setup_project(&[
        ("main.cpp", ""),
        ("sub/helper.cpp", ""),
        ("sub/deep/util.c", ""),
    ]);
    let scanner = SourceScanner::new(&src_dir, &build_dir);
    let sources = scanner.scan_sketch_sources().unwrap();
    assert_eq!(sources.len(), 3);
}

#[test]
fn test_scan_core_sources() {
    let tmp = TempDir::new().unwrap();
    let core_dir = tmp.path().join("cores/arduino");
    fs::create_dir_all(&core_dir).unwrap();
    fs::write(core_dir.join("main.cpp"), "int main() {}").unwrap();
    fs::write(core_dir.join("wiring.c"), "void init() {}").unwrap();
    fs::write(core_dir.join("helper.cxx"), "void helper() {}").unwrap();
    fs::write(core_dir.join("Arduino.h"), "#pragma once").unwrap();

    let scanner = SourceScanner::new(&tmp.path().join("src"), &tmp.path().join("build"));
    let sources = scanner.scan_core_sources(&core_dir);
    assert_eq!(sources.len(), 3); // .cpp, .c, and .cxx, not .h
    assert!(sources.iter().any(|p| p.ends_with("helper.cxx")));
}

#[test]
fn test_nonexistent_core_directory() {
    let tmp = TempDir::new().unwrap();
    let scanner = SourceScanner::new(&tmp.path().join("src"), &tmp.path().join("build"));
    let sources = scanner.scan_core_sources(&tmp.path().join("nonexistent"));
    assert!(sources.is_empty());
}

#[test]
fn test_scan_variant_cxx_sources() {
    let tmp = TempDir::new().unwrap();
    let variant_dir = tmp.path().join("variants/demo");
    fs::create_dir_all(&variant_dir).unwrap();
    fs::write(variant_dir.join("variant.cxx"), "void variant() {}").unwrap();
    fs::write(variant_dir.join("variant.h"), "#pragma once").unwrap();

    let scanner = SourceScanner::new(&tmp.path().join("src"), &tmp.path().join("build"));
    let sources = scanner.scan_variant_sources(&variant_dir);

    assert_eq!(sources, vec![variant_dir.join("variant.cxx")]);
}

#[test]
fn test_preprocess_simple_ino() {
    let (_tmp, src_dir, build_dir) = setup_project(&[(
        "sketch.ino",
        "void setup() {\n  pinMode(13, OUTPUT);\n}\n\nvoid loop() {\n  digitalWrite(13, HIGH);\n}\n",
    )]);
    let scanner = SourceScanner::new(&src_dir, &build_dir);
    let sources = scanner.scan_sketch_sources().unwrap();
    let content = fs::read_to_string(&sources[0]).unwrap();

    assert!(!content.contains("#include <Arduino.h>"));
    assert!(content.contains("void setup()"));
    assert!(content.contains("void loop()"));
}

#[test]
fn test_preprocess_includes_arduino_h_when_header_available() {
    let tmp = TempDir::new().unwrap();
    let src_dir = tmp.path().join("src");
    let build_dir = tmp.path().join("build");
    let core_dir = tmp.path().join("core");
    fs::create_dir_all(&src_dir).unwrap();
    fs::create_dir_all(&core_dir).unwrap();
    fs::write(
        src_dir.join("sketch.ino"),
        "void setup() {}\nvoid loop() {}\n",
    )
    .unwrap();
    fs::write(core_dir.join("Arduino.h"), "#pragma once\n").unwrap();

    let scanner = SourceScanner::new(&src_dir, &build_dir);
    let sources = scanner
        .scan_all(Some(&core_dir), None)
        .unwrap()
        .sketch_sources;
    let content = fs::read_to_string(&sources[0]).unwrap();

    assert!(content.contains("#include <Arduino.h>"));
}

#[test]
fn test_preprocess_with_custom_functions() {
    let (_tmp, src_dir, build_dir) = setup_project(&[(
        "sketch.ino",
        "int add(int a, int b) {\n  return a + b;\n}\n\nvoid setup() {\n  int x = add(1, 2);\n}\n\nvoid loop() {}\n",
    )]);
    let scanner = SourceScanner::new(&src_dir, &build_dir);
    let sources = scanner.scan_sketch_sources().unwrap();
    let content = fs::read_to_string(&sources[0]).unwrap();

    // Should have auto-generated prototypes for custom helpers, but not for
    // Arduino-owned setup()/loop().
    assert!(content.contains("int add(int a, int b)"));
    assert!(!content.contains("void setup();"));
    assert!(!content.contains("void loop();"));
}

#[test]
fn test_preprocess_preserves_existing_forward_declarations() {
    let (_tmp, src_dir, build_dir) = setup_project(&[(
        "sketch.ino",
        "extern void helper();\n\nvoid setup() {\n  helper();\n}\n\nvoid loop() {}\n",
    )]);
    let scanner = SourceScanner::new(&src_dir, &build_dir);
    let sources = scanner.scan_sketch_sources().unwrap();
    let content = fs::read_to_string(&sources[0]).unwrap();

    assert!(content.contains("extern void helper();"));
}

#[test]
fn test_function_prototype_extraction() {
    let source = "void setup() {\n}\nint compute(float x, int y) {\n  return 0;\n}\nconst char* getName() {\n  return \"\";\n}\n";
    let protos = extract_function_prototypes(source);
    assert!(protos.len() >= 2);
    assert!(
        !protos.iter().any(|p| p.contains("setup")),
        "Arduino entry points are declared by Arduino.h and should not be auto-prototyped"
    );
    assert!(protos.iter().any(|p| p.contains("compute")));
}

#[test]
fn test_prototype_extraction_handles_complex_cpp_signatures() {
    let source = r#"
template <typename T>
T twice(T value) {
  return value + value;
}

[[nodiscard]] const char* label(const char* fallback = "demo") {
  return fallback;
}

int& ref_value(int& value) {
  return value;
}
"#;
    let protos = extract_function_prototypes(source);
    assert!(protos.contains(&"template <typename T> T twice(T value)".to_string()));
    assert!(protos.contains(&"[[nodiscard]] const char* label(const char* fallback)".to_string()));
    assert!(protos.contains(&"int& ref_value(int& value)".to_string()));
    assert!(!protos.iter().any(|p| p.contains("= \"demo\"")));
}

#[test]
fn test_prototype_extraction_skips_non_free_functions() {
    let source = r#"
#define MAKE_FUNC(name) void name() {}

void setup() {
  if (true) {
  }
  while (false) {
  }
  auto callback = []() { return 1; };
}

class Controller {
  void tick() {}
};

namespace hidden {
void helper() {}
}

void Controller::external_tick() {}
"#;
    let protos = extract_function_prototypes(source);
    assert!(!protos.iter().any(|p| p == "void setup()"));
    assert!(!protos.iter().any(|p| p.contains("if")));
    assert!(!protos.iter().any(|p| p.contains("while")));
    assert!(!protos.iter().any(|p| p.contains("callback")));
    assert!(!protos.iter().any(|p| p.contains("tick")));
    assert!(!protos.iter().any(|p| p.contains("helper")));
    assert!(!protos.iter().any(|p| p.contains("MAKE_FUNC")));
}

#[test]
fn test_line_numbers_preserved() {
    let (_tmp, src_dir, build_dir) =
        setup_project(&[("sketch.ino", "void setup() {}\nvoid loop() {}\n")]);
    let scanner = SourceScanner::new(&src_dir, &build_dir);
    let sources = scanner.scan_sketch_sources().unwrap();
    let content = fs::read_to_string(&sources[0]).unwrap();
    assert!(content.contains("#line 1"));
}

#[test]
fn test_line_directive_path_is_project_relative_and_slash_normalized() {
    let (_tmp, src_dir, build_dir) =
        setup_project(&[("sketch.ino", "void setup() {}\nvoid loop() {}\n")]);
    let scanner = SourceScanner::new(&src_dir, &build_dir);
    let sources = scanner.scan_sketch_sources().unwrap();
    let content = fs::read_to_string(&sources[0]).unwrap();

    assert!(content.contains("#line 1 \"src/sketch.ino\""));
    assert!(!content.contains('\\'));
}

#[test]
fn test_generated_ino_cpp_uses_lf_line_endings() {
    let (_tmp, src_dir, build_dir) =
        setup_project(&[("sketch.ino", "void setup() {}\r\nvoid loop() {}\r\n")]);
    let scanner = SourceScanner::new(&src_dir, &build_dir);
    let sources = scanner.scan_sketch_sources().unwrap();
    let content = fs::read_to_string(&sources[0]).unwrap();

    assert!(!content.contains("\r\n"));
}

#[test]
fn test_windows_style_generated_path_text_is_stable() {
    assert_eq!(
        normalize_generated_source_path_text(r"C:\Users\dev\project\src\main.ino"),
        "c:/Users/dev/project/src/main.ino"
    );
    assert_eq!(
        normalize_generated_source_path_text(r"C:\Users\dev\project/src\main.ino"),
        "c:/Users/dev/project/src/main.ino"
    );
    assert_eq!(
        normalize_generated_source_path_text("src\\main.ino"),
        "src/main.ino"
    );
}

#[test]
fn test_preprocess_does_not_rewrite_unchanged_output() {
    let (_tmp, src_dir, build_dir) =
        setup_project(&[("sketch.ino", "void setup() {}\nvoid loop() {}\n")]);
    let scanner = SourceScanner::new(&src_dir, &build_dir);

    let first = scanner.scan_sketch_sources().unwrap();
    let output = first[0].clone();
    let first_mtime = fs::metadata(&output).unwrap().modified().unwrap();

    std::thread::sleep(std::time::Duration::from_millis(20));

    let second = scanner.scan_sketch_sources().unwrap();
    assert_eq!(second[0], output);
    let second_mtime = fs::metadata(&output).unwrap().modified().unwrap();

    assert_eq!(first_mtime, second_mtime);
}

#[test]
fn test_preprocess_with_arduino_h_does_not_rewrite_unchanged_output() {
    let tmp = TempDir::new().unwrap();
    let src_dir = tmp.path().join("src");
    let build_dir = tmp.path().join("build");
    let core_dir = tmp.path().join("core");
    fs::create_dir_all(&src_dir).unwrap();
    fs::create_dir_all(&core_dir).unwrap();
    fs::write(
        src_dir.join("sketch.ino"),
        "void setup() {}\nvoid loop() {}\n",
    )
    .unwrap();
    fs::write(core_dir.join("Arduino.h"), "#pragma once\n").unwrap();
    let scanner = SourceScanner::new(&src_dir, &build_dir);

    let first = scanner
        .scan_all(Some(&core_dir), None)
        .unwrap()
        .sketch_sources;
    let output = first[0].clone();
    let first_mtime = fs::metadata(&output).unwrap().modified().unwrap();

    std::thread::sleep(std::time::Duration::from_millis(20));

    let second = scanner
        .scan_all(Some(&core_dir), None)
        .unwrap()
        .sketch_sources;
    assert_eq!(second[0], output);
    let second_mtime = fs::metadata(&output).unwrap().modified().unwrap();

    assert_eq!(first_mtime, second_mtime);
}

#[test]
fn test_source_collection_all_sources() {
    let tmp = TempDir::new().unwrap();
    let src_dir = tmp.path().join("src");
    let core_dir = tmp.path().join("core");
    let variant_dir = tmp.path().join("variant");
    let build_dir = tmp.path().join("build");
    fs::create_dir_all(&src_dir).unwrap();
    fs::create_dir_all(&core_dir).unwrap();
    fs::create_dir_all(&variant_dir).unwrap();
    fs::write(src_dir.join("main.cpp"), "").unwrap();
    fs::write(core_dir.join("core.cpp"), "").unwrap();
    fs::write(variant_dir.join("variant.c"), "").unwrap();

    let scanner = SourceScanner::new(&src_dir, &build_dir);
    let collection = scanner
        .scan_all(Some(&core_dir), Some(&variant_dir))
        .unwrap();
    assert_eq!(collection.sketch_sources.len(), 1);
    assert_eq!(collection.core_sources.len(), 1);
    assert_eq!(collection.variant_sources.len(), 1);
    assert_eq!(collection.all_sources().len(), 3);
}

#[test]
fn test_scan_sketch_sources_filtered_excludes_subdirectory() {
    let (_tmp, src_dir, build_dir) = setup_project(&[
        ("main.cpp", "int main() { return 0; }"),
        ("generated/skip.cpp", "void skip() {}"),
    ]);
    let scanner = SourceScanner::new(&src_dir, &build_dir);
    let sources = scanner
        .scan_sketch_sources_filtered(Some("+<*>\n-<generated/>"))
        .unwrap();
    assert_eq!(sources.len(), 1);
    assert!(sources[0].ends_with("main.cpp"));
}

#[test]
fn test_scan_sketch_sources_filtered_includes_only_selected_files() {
    let (_tmp, src_dir, build_dir) = setup_project(&[
        ("main.cpp", "int main() { return 0; }"),
        ("helper.cpp", "void helper() {}"),
        ("sub/util.c", "void util() {}"),
    ]);
    let scanner = SourceScanner::new(&src_dir, &build_dir);
    let sources = scanner
        .scan_sketch_sources_filtered(Some("+<main.cpp>\n+<sub/util.c>"))
        .unwrap();
    assert_eq!(sources.len(), 2);
    assert!(sources.iter().any(|p| p.ends_with("main.cpp")));
    assert!(sources
        .iter()
        .any(|p| p.ends_with("sub\\util.c") || p.ends_with("sub/util.c")));
    assert!(!sources.iter().any(|p| p.ends_with("helper.cpp")));
}
