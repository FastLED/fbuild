//! Clang flag translation and IWYU preparation tests.

use std::path::{Path, PathBuf};

use super::super::clang::{isystem_args_from_dirs, should_remove_flag};
use crate::compile_database::{
    CompileDatabase, CompileEntry, TargetArchitecture, translate_flags_for_clang,
};

#[test]
fn test_target_triples() {
    assert_eq!(TargetArchitecture::Xtensa.target_triple(), "xtensa-esp-elf");
    assert_eq!(
        TargetArchitecture::Riscv32.target_triple(),
        "riscv32-esp-elf"
    );
    assert_eq!(TargetArchitecture::Avr.target_triple(), "avr");
    assert_eq!(TargetArchitecture::Arm.target_triple(), "arm-none-eabi");
}

#[test]
fn test_translate_gcc_to_clang() {
    let args = vec![
        "/usr/bin/avr-gcc".to_string(),
        "-Os".to_string(),
        "-c".to_string(),
    ];
    let result = translate_flags_for_clang(&args, TargetArchitecture::Avr);
    assert_eq!(result[0], "clang");
}

#[test]
fn test_translate_gxx_to_clangxx() {
    let args = vec!["/usr/bin/arm-none-eabi-g++".to_string(), "-Os".to_string()];
    let result = translate_flags_for_clang(&args, TargetArchitecture::Arm);
    assert_eq!(result[0], "clang++");
}

#[test]
fn test_translate_windows_compiler_path() {
    let args = vec![
        "C:\\tools\\xtensa-esp32-elf-g++.exe".to_string(),
        "-Os".to_string(),
    ];
    let result = translate_flags_for_clang(&args, TargetArchitecture::Xtensa);
    assert_eq!(result[0], "clang++");
}

#[test]
fn test_translate_adds_target() {
    let args = vec!["/usr/bin/gcc".to_string(), "-c".to_string()];
    let result = translate_flags_for_clang(&args, TargetArchitecture::Xtensa);
    assert_eq!(result[1], "--target=xtensa-esp-elf");
}

#[test]
fn test_translate_removes_common_lto_flags() {
    let args = vec![
        "/usr/bin/gcc".to_string(),
        "-flto=auto".to_string(),
        "-flto".to_string(),
        "-fno-fat-lto-objects".to_string(),
        "-fuse-linker-plugin".to_string(),
        "-ffat-lto-objects".to_string(),
        "-Os".to_string(),
    ];
    let result = translate_flags_for_clang(&args, TargetArchitecture::Avr);
    assert!(!result.contains(&"-flto=auto".to_string()));
    assert!(!result.contains(&"-flto".to_string()));
    assert!(!result.contains(&"-fno-fat-lto-objects".to_string()));
    assert!(!result.contains(&"-fuse-linker-plugin".to_string()));
    assert!(!result.contains(&"-ffat-lto-objects".to_string()));
    assert!(result.contains(&"-Os".to_string()));
}

#[test]
fn test_translate_xtensa_removals() {
    let args = vec![
        "/usr/bin/xtensa-esp32-elf-gcc".to_string(),
        "-mlongcalls".to_string(),
        "-mdisable-hardware-atomics".to_string(),
        "-mfix-esp32-psram-cache-issue".to_string(),
        "-fstrict-volatile-bitfields".to_string(),
        "-mtext-section-literals".to_string(),
        "-fno-tree-switch-conversion".to_string(),
        "-Os".to_string(),
    ];
    let result = translate_flags_for_clang(&args, TargetArchitecture::Xtensa);
    assert!(!result.contains(&"-mlongcalls".to_string()));
    assert!(!result.contains(&"-mdisable-hardware-atomics".to_string()));
    assert!(!result.contains(&"-mfix-esp32-psram-cache-issue".to_string()));
    assert!(!result.contains(&"-fstrict-volatile-bitfields".to_string()));
    assert!(!result.contains(&"-mtext-section-literals".to_string()));
    assert!(!result.contains(&"-fno-tree-switch-conversion".to_string()));
    assert!(result.contains(&"-Os".to_string()));
}

#[test]
fn test_translate_xtensa_psram_strategy_prefix() {
    let args = vec![
        "/usr/bin/gcc".to_string(),
        "-mfix-esp32-psram-cache-strategy=memw".to_string(),
        "-Os".to_string(),
    ];
    let result = translate_flags_for_clang(&args, TargetArchitecture::Xtensa);
    assert!(!result.contains(&"-mfix-esp32-psram-cache-strategy=memw".to_string()));
    assert!(result.contains(&"-Os".to_string()));
}

#[test]
fn test_translate_riscv_removals() {
    let args = vec![
        "/usr/bin/riscv32-esp-elf-gcc".to_string(),
        "-mabi=ilp32".to_string(),
        "-mno-fdiv".to_string(),
        "-march=rv32imac".to_string(),
        "-Os".to_string(),
    ];
    let result = translate_flags_for_clang(&args, TargetArchitecture::Riscv32);
    assert!(!result.contains(&"-mabi=ilp32".to_string()));
    assert!(!result.contains(&"-mno-fdiv".to_string()));
    assert!(result.contains(&"-march=rv32imac".to_string()));
}

#[test]
fn test_translate_arm_removals() {
    let args = vec![
        "/usr/bin/arm-none-eabi-g++".to_string(),
        "-mthumb-interwork".to_string(),
        "-mcpu=cortex-m7".to_string(),
        "-Os".to_string(),
    ];
    let result = translate_flags_for_clang(&args, TargetArchitecture::Arm);
    assert!(!result.contains(&"-mthumb-interwork".to_string()));
    assert!(result.contains(&"-mcpu=cortex-m7".to_string()));
}

#[test]
fn test_translate_avr_no_extra_removals() {
    let args = vec![
        "/usr/bin/avr-gcc".to_string(),
        "-mmcu=atmega328p".to_string(),
        "-Os".to_string(),
    ];
    let result = translate_flags_for_clang(&args, TargetArchitecture::Avr);
    assert!(result.contains(&"-mmcu=atmega328p".to_string()));
    assert!(result.contains(&"-Os".to_string()));
}

#[test]
fn test_translate_preserves_includes_and_defines() {
    let args = vec![
        "/usr/bin/gcc".to_string(),
        "-I/path/to/include".to_string(),
        "-DFOO=1".to_string(),
        "-c".to_string(),
    ];
    let result = translate_flags_for_clang(&args, TargetArchitecture::Avr);
    assert!(result.contains(&"-I/path/to/include".to_string()));
    assert!(result.contains(&"-DFOO=1".to_string()));
}

#[test]
fn test_translate_empty_args() {
    let args: Vec<String> = vec![];
    let result = translate_flags_for_clang(&args, TargetArchitecture::Avr);
    assert!(result.is_empty());
}

#[test]
fn test_database_translate_for_clang() {
    let mut db = CompileDatabase::new();
    db.add_entry(CompileEntry {
        arguments: vec![
            "/usr/bin/xtensa-esp32-elf-gcc".to_string(),
            "-mlongcalls".to_string(),
            "-Os".to_string(),
            "-c".to_string(),
            "main.c".to_string(),
        ],
        directory: "/project".to_string(),
        file: "main.c".to_string(),
        output: Some("main.o".to_string()),
    });
    db.add_entry(CompileEntry {
        arguments: vec![
            "/usr/bin/xtensa-esp32-elf-g++".to_string(),
            "-mlongcalls".to_string(),
            "-std=c++17".to_string(),
            "-c".to_string(),
            "app.cpp".to_string(),
        ],
        directory: "/project".to_string(),
        file: "app.cpp".to_string(),
        output: Some("app.o".to_string()),
    });

    let translated = db.translate_for_clang(TargetArchitecture::Xtensa);

    // First entry: gcc → clang
    assert_eq!(translated.entries[0].arguments[0], "clang");
    assert_eq!(
        translated.entries[0].arguments[1],
        "--target=xtensa-esp-elf"
    );
    assert!(
        !translated.entries[0]
            .arguments
            .contains(&"-mlongcalls".to_string())
    );
    assert!(translated.entries[0].arguments.contains(&"-Os".to_string()));
    assert_eq!(translated.entries[0].file, "main.c");

    // Second entry: g++ → clang++
    assert_eq!(translated.entries[1].arguments[0], "clang++");
    assert!(
        !translated.entries[1]
            .arguments
            .contains(&"-mlongcalls".to_string())
    );
    assert!(
        translated.entries[1]
            .arguments
            .contains(&"-std=c++17".to_string())
    );
}

// =========================================================================
// FastLED/fbuild#1076 Phase 0: builtin include-dir baking + raw-.ino swap
// =========================================================================

#[test]
fn test_isystem_args_from_dirs_sorted_and_deduped() {
    let dirs = vec![
        PathBuf::from("/tc/lib/gcc/xtensa/14/include"),
        PathBuf::from("/tc/lib/gcc/avr/14/include"),
        PathBuf::from("/tc/lib/gcc/xtensa/14/include"), // duplicate
    ];
    let args = isystem_args_from_dirs(dirs);
    assert_eq!(
        args,
        vec![
            "-isystem".to_string(),
            "/tc/lib/gcc/avr/14/include".to_string(),
            "-isystem".to_string(),
            "/tc/lib/gcc/xtensa/14/include".to_string(),
        ]
    );
}

#[test]
fn test_isystem_args_from_dirs_empty_is_no_op() {
    assert!(isystem_args_from_dirs(Vec::new()).is_empty());
}

fn ino_cpp_template_entry() -> CompileEntry {
    CompileEntry {
        arguments: vec![
            "clang++".to_string(),
            "--target=xtensa-esp-elf".to_string(),
            "-Isrc".to_string(),
            "-c".to_string(),
            "/project/.fbuild/build/env/src/sketch.ino.cpp".to_string(),
            "-o".to_string(),
            "/project/.fbuild/build/env/src/sketch.ino.cpp.o".to_string(),
        ],
        directory: "/project".to_string(),
        file: "/project/.fbuild/build/env/src/sketch.ino.cpp".to_string(),
        output: Some("/project/.fbuild/build/env/src/sketch.ino.cpp.o".to_string()),
    }
}

#[test]
fn test_swap_ino_entries_for_raw_replaces_generated_entry() {
    let mut db = CompileDatabase::new();
    db.add_entry(ino_cpp_template_entry());

    let ino_preludes = vec![(
        PathBuf::from("/project/src/sketch.ino"),
        PathBuf::from("/project/.fbuild/build/env/src/sketch.ino.prelude.h"),
    )];
    let swapped = db.swap_ino_entries_for_raw(&ino_preludes);

    // Generated .ino.cpp entry removed.
    assert!(!swapped.entries.iter().any(|e| e.file.ends_with(".ino.cpp")));

    // Raw .ino entry present with -x c++ -include <prelude> and the raw
    // path swapped in for both `file` and the `-c <file>` argument.
    assert_eq!(swapped.entries.len(), 1);
    let entry = &swapped.entries[0];
    assert_eq!(entry.file, "/project/src/sketch.ino");
    assert!(entry.arguments.contains(&"-x".to_string()));
    assert!(entry.arguments.contains(&"c++".to_string()));
    let include_idx = entry
        .arguments
        .iter()
        .position(|a| a == "-include")
        .unwrap();
    assert_eq!(
        entry.arguments[include_idx + 1],
        "/project/.fbuild/build/env/src/sketch.ino.prelude.h"
    );
    assert!(
        entry
            .arguments
            .contains(&"/project/src/sketch.ino".to_string())
    );
    assert!(!entry.arguments.iter().any(|a| a.ends_with(".ino.cpp")));
    // Flags from the template are preserved.
    assert!(entry.arguments.contains(&"-Isrc".to_string()));
    assert!(
        entry
            .arguments
            .contains(&"--target=xtensa-esp-elf".to_string())
    );
}

#[test]
fn test_swap_ino_entries_for_raw_multi_tab() {
    let mut db = CompileDatabase::new();
    db.add_entry(ino_cpp_template_entry());

    let ino_preludes = vec![
        (
            PathBuf::from("/project/src/main.ino"),
            PathBuf::from("/project/.fbuild/build/env/src/main.ino.prelude.h"),
        ),
        (
            PathBuf::from("/project/src/a_tab.ino"),
            PathBuf::from("/project/.fbuild/build/env/src/a_tab.ino.prelude.h"),
        ),
    ];
    let swapped = db.swap_ino_entries_for_raw(&ino_preludes);

    assert_eq!(swapped.entries.len(), 2);
    assert!(
        swapped
            .entries
            .iter()
            .any(|e| e.file == "/project/src/main.ino")
    );
    assert!(
        swapped
            .entries
            .iter()
            .any(|e| e.file == "/project/src/a_tab.ino")
    );
    assert!(!swapped.entries.iter().any(|e| e.file.ends_with(".ino.cpp")));
}

#[test]
fn test_swap_ino_entries_for_raw_empty_preludes_is_no_op() {
    let mut db = CompileDatabase::new();
    db.add_entry(ino_cpp_template_entry());

    let swapped = db.swap_ino_entries_for_raw(&[]);
    // The generated entry survives untouched — this is the main.cpp-present
    // / no-.ino-tabs case, where there is nothing to swap.
    assert_eq!(swapped.entries.len(), 1);
    assert!(swapped.entries[0].file.ends_with(".ino.cpp"));
}

#[test]
fn test_swap_ino_entries_for_raw_no_generated_entry_leaves_db_untouched() {
    let mut db = CompileDatabase::new();
    db.add_entry(CompileEntry {
        arguments: vec!["clang".to_string(), "-c".to_string(), "main.c".to_string()],
        directory: "/project".to_string(),
        file: "main.c".to_string(),
        output: None,
    });

    let ino_preludes = vec![(
        PathBuf::from("/project/src/sketch.ino"),
        PathBuf::from("/project/.fbuild/build/env/src/sketch.ino.prelude.h"),
    )];
    let swapped = db.swap_ino_entries_for_raw(&ino_preludes);
    assert_eq!(swapped.entries.len(), 1);
    assert_eq!(swapped.entries[0].file, "main.c");
}

#[test]
fn test_translate_does_not_modify_original() {
    let mut db = CompileDatabase::new();
    db.add_entry(CompileEntry {
        arguments: vec![
            "/usr/bin/gcc".to_string(),
            "-mlongcalls".to_string(),
            "-Os".to_string(),
        ],
        directory: "/project".to_string(),
        file: "main.c".to_string(),
        output: None,
    });

    let _translated = db.translate_for_clang(TargetArchitecture::Xtensa);

    // Original should still have -mlongcalls
    assert!(db.entries[0].arguments.contains(&"-mlongcalls".to_string()));
    assert_eq!(db.entries[0].arguments[0], "/usr/bin/gcc");
}

// =========================================================================
// IWYU preparation tests
// =========================================================================

#[test]
fn test_should_remove_freorder_blocks() {
    assert!(should_remove_flag(
        "-freorder-blocks",
        TargetArchitecture::Xtensa
    ));
    assert!(should_remove_flag(
        "-freorder-blocks",
        TargetArchitecture::Avr
    ));
}

#[test]
fn test_should_remove_fno_jump_tables() {
    assert!(should_remove_flag(
        "-fno-jump-tables",
        TargetArchitecture::Xtensa
    ));
}

#[test]
fn test_fstack_protector_preserved() {
    // -fstack-protector is supported by clang — keep it
    assert!(!should_remove_flag(
        "-fstack-protector",
        TargetArchitecture::Xtensa
    ));
}

#[test]
fn test_prepare_for_iwyu_removes_target() {
    let mut db = CompileDatabase::new();
    db.add_entry(CompileEntry {
        arguments: vec![
            "clang++".into(),
            "--target=xtensa-esp-elf".into(),
            "-Os".into(),
            "-c".into(),
            "src/main.cpp".into(),
        ],
        directory: "/project".into(),
        file: "src/main.cpp".into(),
        output: None,
    });
    let result = db.prepare_for_iwyu(Path::new("/project/src"), &[]);
    assert!(
        !result.entries[0]
            .arguments
            .iter()
            .any(|a| a.starts_with("--target="))
    );
}

#[test]
fn test_prepare_for_iwyu_dedup_defines() {
    let mut db = CompileDatabase::new();
    db.add_entry(CompileEntry {
        arguments: vec![
            "clang".into(),
            "-DFOO=1".into(),
            "-DBAR".into(),
            "-DFOO=2".into(), // duplicate, should be dropped
        ],
        directory: "/project".into(),
        file: "src/main.c".into(),
        output: None,
    });
    let result = db.prepare_for_iwyu(Path::new("/project/src"), &[]);
    let defines: Vec<&str> = result.entries[0]
        .arguments
        .iter()
        .filter(|a| a.starts_with("-D"))
        .map(|a| a.as_str())
        .collect();
    assert_eq!(defines, vec!["-DFOO=1", "-DBAR"]);
}

#[test]
fn test_prepare_for_iwyu_converts_system_includes() {
    let mut db = CompileDatabase::new();
    db.add_entry(CompileEntry {
        arguments: vec![
            "clang".into(),
            "-I/project/src/mylib".into(),
            "-I/usr/include/esp32".into(),
        ],
        directory: "/project".into(),
        file: "src/main.c".into(),
        output: None,
    });
    let result = db.prepare_for_iwyu(Path::new("/project/src"), &[]);
    let args = &result.entries[0].arguments;
    // Project include kept as -I
    assert!(args.contains(&"-I/project/src/mylib".to_string()));
    // System include converted to -isystem
    assert!(args.contains(&"-isystem".to_string()));
    assert!(args.contains(&"/usr/include/esp32".to_string()));
    assert!(!args.contains(&"-I/usr/include/esp32".to_string()));
}

#[test]
fn test_prepare_for_iwyu_adds_extra_system_includes() {
    let mut db = CompileDatabase::new();
    db.add_entry(CompileEntry {
        arguments: vec!["clang".into(), "-c".into(), "src/main.c".into()],
        directory: "/project".into(),
        file: "src/main.c".into(),
        output: None,
    });
    let extras = vec![PathBuf::from("/toolchain/lib/gcc/xtensa/14/include")];
    let result = db.prepare_for_iwyu(Path::new("/project/src"), &extras);
    let args = &result.entries[0].arguments;
    assert!(args.contains(&"-isystem".to_string()));
    assert!(args.contains(&"/toolchain/lib/gcc/xtensa/14/include".to_string()));
}
