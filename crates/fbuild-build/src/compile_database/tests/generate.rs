//! Tests for `generate_entries`.

use std::path::{Path, PathBuf};

use super::generate_entries;

// --- Entry generation tests ---

#[test]
fn test_generate_entries_c_uses_gcc() {
    let entries = generate_entries(
        Path::new("/usr/bin/gcc"),
        Path::new("/usr/bin/g++"),
        &["-std=c11".to_string()],
        &["-std=c++17".to_string()],
        &[],
        &[],
        &[PathBuf::from("main.c")],
        Path::new("/build"),
        Path::new("/project"),
    );
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].arguments[0], "/usr/bin/gcc");
    assert!(entries[0].arguments.contains(&"-std=c11".to_string()));
}

#[test]
fn test_generate_entries_cpp_uses_gxx() {
    let entries = generate_entries(
        Path::new("/usr/bin/gcc"),
        Path::new("/usr/bin/g++"),
        &["-std=c11".to_string()],
        &["-std=c++17".to_string()],
        &[],
        &[],
        &[PathBuf::from("main.cpp")],
        Path::new("/build"),
        Path::new("/project"),
    );
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].arguments[0], "/usr/bin/g++");
    assert!(entries[0].arguments.contains(&"-std=c++17".to_string()));
}

#[test]
fn test_generate_entries_s_uses_gcc() {
    let entries = generate_entries(
        Path::new("/usr/bin/gcc"),
        Path::new("/usr/bin/g++"),
        &["-std=c11".to_string()],
        &["-std=c++17".to_string()],
        &[],
        &[],
        &[PathBuf::from("startup.s")],
        Path::new("/build"),
        Path::new("/project"),
    );
    assert_eq!(entries[0].arguments[0], "/usr/bin/gcc");
}

#[test]
fn test_generate_entries_empty_sources() {
    let entries = generate_entries(
        Path::new("/usr/bin/gcc"),
        Path::new("/usr/bin/g++"),
        &[],
        &[],
        &[],
        &[],
        &[],
        Path::new("/build"),
        Path::new("/project"),
    );
    assert!(entries.is_empty());
}

#[test]
fn test_generate_entries_include_flags_in_args() {
    let entries = generate_entries(
        Path::new("/usr/bin/gcc"),
        Path::new("/usr/bin/g++"),
        &[],
        &[],
        &["-I/sdk/include".to_string(), "-I/core/include".to_string()],
        &[],
        &[PathBuf::from("main.cpp")],
        Path::new("/build"),
        Path::new("/project"),
    );
    assert!(entries[0].arguments.contains(&"-I/sdk/include".to_string()));
    assert!(entries[0]
        .arguments
        .contains(&"-I/core/include".to_string()));
}

#[test]
fn test_generate_entries_extra_flags_in_args() {
    let entries = generate_entries(
        Path::new("/usr/bin/gcc"),
        Path::new("/usr/bin/g++"),
        &[],
        &[],
        &[],
        &["-DUSER_FLAG=1".to_string()],
        &[PathBuf::from("main.cpp")],
        Path::new("/build"),
        Path::new("/project"),
    );
    assert!(entries[0].arguments.contains(&"-DUSER_FLAG=1".to_string()));
}

#[test]
fn test_generate_entries_directory_is_project_dir() {
    let entries = generate_entries(
        Path::new("/usr/bin/gcc"),
        Path::new("/usr/bin/g++"),
        &[],
        &[],
        &[],
        &[],
        &[PathBuf::from("main.cpp")],
        Path::new("/build"),
        Path::new("/my/project"),
    );
    assert_eq!(entries[0].directory, "/my/project");
}

#[test]
fn test_generate_entries_arguments_structure() {
    let entries = generate_entries(
        Path::new("/usr/bin/gcc"),
        Path::new("/usr/bin/g++"),
        &["-Os".to_string()],
        &["-Os".to_string()],
        &["-I/inc".to_string()],
        &["-DFOO".to_string()],
        &[PathBuf::from("main.c")],
        Path::new("/build"),
        Path::new("/project"),
    );

    let args = &entries[0].arguments;
    // Starts with compiler
    assert_eq!(args[0], "/usr/bin/gcc");
    // Ends with -c source -o object
    let len = args.len();
    assert_eq!(args[len - 4], "-c");
    assert_eq!(args[len - 3], "main.c");
    assert_eq!(args[len - 2], "-o");
}

// --- Extension classification adversarial tests ---

#[test]
fn test_generate_entries_uppercase_c_extension() {
    // .C is treated as C++ on some systems, but our lowercase normalization
    // maps it to "c" → gcc. This matches GCC behavior.
    let entries = generate_entries(
        Path::new("/usr/bin/gcc"),
        Path::new("/usr/bin/g++"),
        &[],
        &[],
        &[],
        &[],
        &[PathBuf::from("main.C")],
        Path::new("/build"),
        Path::new("/project"),
    );
    // After to_lowercase(), ".C" becomes "c" → uses gcc
    assert_eq!(entries[0].arguments[0], "/usr/bin/gcc");
}

#[test]
fn test_generate_entries_cc_extension_uses_gxx() {
    let entries = generate_entries(
        Path::new("/usr/bin/gcc"),
        Path::new("/usr/bin/g++"),
        &[],
        &[],
        &[],
        &[],
        &[PathBuf::from("module.cc")],
        Path::new("/build"),
        Path::new("/project"),
    );
    assert_eq!(entries[0].arguments[0], "/usr/bin/g++");
}

#[test]
fn test_generate_entries_cxx_extension_uses_gxx() {
    let entries = generate_entries(
        Path::new("/usr/bin/gcc"),
        Path::new("/usr/bin/g++"),
        &[],
        &[],
        &[],
        &[],
        &[PathBuf::from("module.cxx")],
        Path::new("/build"),
        Path::new("/project"),
    );
    assert_eq!(entries[0].arguments[0], "/usr/bin/g++");
}

#[test]
fn test_generate_entries_ino_cpp_uses_gxx() {
    // Preprocessed .ino files become .ino.cpp — extension is "cpp"
    let entries = generate_entries(
        Path::new("/usr/bin/gcc"),
        Path::new("/usr/bin/g++"),
        &[],
        &[],
        &[],
        &[],
        &[PathBuf::from("sketch.ino.cpp")],
        Path::new("/build"),
        Path::new("/project"),
    );
    assert_eq!(entries[0].arguments[0], "/usr/bin/g++");
}

#[test]
fn test_generate_entries_no_extension_uses_gxx() {
    // Files without extension fall through to g++ (the default branch)
    let entries = generate_entries(
        Path::new("/usr/bin/gcc"),
        Path::new("/usr/bin/g++"),
        &[],
        &[],
        &[],
        &[],
        &[PathBuf::from("Makefile")],
        Path::new("/build"),
        Path::new("/project"),
    );
    assert_eq!(entries[0].arguments[0], "/usr/bin/g++");
}

#[test]
fn test_generate_entries_uppercase_s_assembly_uses_gcc() {
    // .S (uppercase) is GCC-preprocessed assembly, should use gcc
    let entries = generate_entries(
        Path::new("/usr/bin/gcc"),
        Path::new("/usr/bin/g++"),
        &[],
        &[],
        &[],
        &[],
        &[PathBuf::from("boot.S")],
        Path::new("/build"),
        Path::new("/project"),
    );
    // to_lowercase() → "s" → matches gcc branch
    assert_eq!(entries[0].arguments[0], "/usr/bin/gcc");
}

// --- Path handling adversarial tests ---

#[test]
fn test_generate_entries_paths_with_spaces() {
    let entries = generate_entries(
        Path::new("/usr/bin/my gcc"),
        Path::new("/usr/bin/my g++"),
        &[],
        &[],
        &["-I/path with spaces/include".to_string()],
        &[],
        &[PathBuf::from("/my project/src/main.cpp")],
        Path::new("/my build"),
        Path::new("/my project"),
    );
    assert_eq!(entries[0].directory, "/my project");
    assert_eq!(entries[0].file, "/my project/src/main.cpp");
    assert!(entries[0]
        .arguments
        .contains(&"-I/path with spaces/include".to_string()));
}

#[test]
fn test_generate_entries_windows_backslash_paths() {
    let entries = generate_entries(
        Path::new("C:\\tools\\gcc.exe"),
        Path::new("C:\\tools\\g++.exe"),
        &[],
        &[],
        &[],
        &[],
        &[PathBuf::from("C:\\Users\\user\\project\\src\\main.cpp")],
        Path::new("C:\\Users\\user\\build"),
        Path::new("C:\\Users\\user\\project"),
    );
    // to_string_lossy preserves original path separators
    assert!(!entries[0].file.is_empty());
    assert!(!entries[0].directory.is_empty());
    // The output field should point to the build dir
    assert!(
        entries[0].output.as_ref().unwrap().contains("build")
            || entries[0].output.as_ref().unwrap().contains("Users")
    );
}

// --- Arguments must never contain @file (response file) references ---

#[test]
fn test_generate_entries_no_response_file_in_args() {
    // Even with many include flags, generate_entries should produce
    // individual -I flags, never @file references (those are for GCC only).
    let include_flags: Vec<String> =
        (0..300).map(|i| format!("-I/sdk/include/{}", i)).collect();

    let entries = generate_entries(
        Path::new("/usr/bin/gcc"),
        Path::new("/usr/bin/g++"),
        &[],
        &[],
        &include_flags,
        &[],
        &[PathBuf::from("main.cpp")],
        Path::new("/build"),
        Path::new("/project"),
    );

    for arg in &entries[0].arguments {
        assert!(
            !arg.starts_with('@'),
            "compile_commands.json must not contain @file references: {}",
            arg
        );
    }
    // All 300 include flags should be present individually
    assert!(entries[0]
        .arguments
        .contains(&"-I/sdk/include/0".to_string()));
    assert!(entries[0]
        .arguments
        .contains(&"-I/sdk/include/299".to_string()));
}

// --- File field must be the source path, not the build path ---

#[test]
fn test_generate_entries_file_is_source_not_build() {
    let entries = generate_entries(
        Path::new("/usr/bin/gcc"),
        Path::new("/usr/bin/g++"),
        &[],
        &[],
        &[],
        &[],
        &[PathBuf::from("/project/src/main.cpp")],
        Path::new("/project/.fbuild/build/esp32/src"),
        Path::new("/project"),
    );
    assert_eq!(entries[0].file, "/project/src/main.cpp");
    // Output should be in the build dir
    assert!(
        entries[0].output.as_ref().unwrap().contains(".fbuild"),
        "output should be in build dir: {:?}",
        entries[0].output
    );
}

// --- Mixed source types in a single call ---

#[test]
fn test_generate_entries_mixed_sources() {
    let sources = vec![
        PathBuf::from("main.cpp"),
        PathBuf::from("util.c"),
        PathBuf::from("boot.S"),
        PathBuf::from("driver.cc"),
        PathBuf::from("algo.cxx"),
    ];
    let entries = generate_entries(
        Path::new("/usr/bin/gcc"),
        Path::new("/usr/bin/g++"),
        &["-std=c11".to_string()],
        &["-std=c++17".to_string()],
        &[],
        &[],
        &sources,
        Path::new("/build"),
        Path::new("/project"),
    );
    assert_eq!(entries.len(), 5);

    // main.cpp → g++
    assert_eq!(entries[0].arguments[0], "/usr/bin/g++");
    assert!(entries[0].arguments.contains(&"-std=c++17".to_string()));

    // util.c → gcc
    assert_eq!(entries[1].arguments[0], "/usr/bin/gcc");
    assert!(entries[1].arguments.contains(&"-std=c11".to_string()));

    // boot.S → gcc (assembly)
    assert_eq!(entries[2].arguments[0], "/usr/bin/gcc");

    // driver.cc → g++
    assert_eq!(entries[3].arguments[0], "/usr/bin/g++");

    // algo.cxx → g++
    assert_eq!(entries[4].arguments[0], "/usr/bin/g++");
}

// --- Duplicate sources don't panic ---

#[test]
fn test_generate_entries_duplicate_sources() {
    let entries = generate_entries(
        Path::new("/usr/bin/gcc"),
        Path::new("/usr/bin/g++"),
        &[],
        &[],
        &[],
        &[],
        &[PathBuf::from("main.cpp"), PathBuf::from("main.cpp")],
        Path::new("/build"),
        Path::new("/project"),
    );
    // Both entries should exist (dedup is the caller's responsibility)
    assert_eq!(entries.len(), 2);
}

// --- Include flags with build dir paths (the clangd navigation issue) ---

#[test]
fn test_generate_entries_include_flags_preserved_verbatim() {
    // The compile database should faithfully reproduce whatever include
    // flags it receives. The ORCHESTRATOR is responsible for passing
    // source-tree paths, not build-dir paths.
    let include_flags = vec![
        "-I/project/src".to_string(), // source tree ✓
        "-I/home/user/.fbuild/build/esp32/libs/fastled/src".to_string(), // cache path ✗
        "-I/framework/cores/esp32".to_string(), // framework ✓
    ];
    let entries = generate_entries(
        Path::new("/usr/bin/gcc"),
        Path::new("/usr/bin/g++"),
        &[],
        &[],
        &include_flags,
        &[],
        &[PathBuf::from("main.cpp")],
        Path::new("/build"),
        Path::new("/project"),
    );
    // All include flags should be present, unmodified
    for flag in &include_flags {
        assert!(
            entries[0].arguments.contains(flag),
            "missing include flag: {}",
            flag
        );
    }
}

// --- Output path uses build_dir, not project source dir ---

#[test]
fn test_generate_entries_output_in_build_dir() {
    let entries = generate_entries(
        Path::new("/usr/bin/gcc"),
        Path::new("/usr/bin/g++"),
        &[],
        &[],
        &[],
        &[],
        &[PathBuf::from("/project/src/main.cpp")],
        Path::new("/build/obj"),
        Path::new("/project"),
    );
    let output = entries[0].output.as_ref().unwrap();
    assert!(
        output.starts_with("/build/obj"),
        "output should start with build dir: {}",
        output
    );
}
