//! Unit tests for CLI argument normalization and `fbuild ci` parsing.

use super::args::{Cli, Commands};
use super::compile_many::{build_ci_pio_env, normalize_ci_sketch_entry, normalize_ci_sketches};
use clap::Parser;

#[test]
fn normalize_ino_path_strips_to_parent_dir() {
    let entry = "examples/Blink/Blink.ino";
    let got = normalize_ci_sketch_entry(entry);
    assert_eq!(
        got,
        std::path::Path::new("examples/Blink").to_string_lossy()
    );
}

#[test]
fn normalize_ino_path_is_case_insensitive() {
    let entry = "examples/Blink/Blink.INO";
    let got = normalize_ci_sketch_entry(entry);
    assert_eq!(
        got,
        std::path::Path::new("examples/Blink").to_string_lossy()
    );
}

#[test]
fn normalize_passes_through_project_dirs() {
    let entry = "examples/Blink";
    assert_eq!(normalize_ci_sketch_entry(entry), "examples/Blink");
}

#[test]
fn normalize_bare_ino_becomes_dot() {
    let entry = "Blink.ino";
    assert_eq!(normalize_ci_sketch_entry(entry), ".");
}

#[test]
fn normalize_batch_preserves_order() {
    let entries = vec![
        "examples/Blink/Blink.ino".to_string(),
        "examples/Fire2012".to_string(),
    ];
    let got = normalize_ci_sketches(&entries);
    assert_eq!(got.len(), 2);
    assert_eq!(
        got[0],
        std::path::Path::new("examples/Blink").to_string_lossy()
    );
    assert_eq!(got[1], "examples/Fire2012");
}

#[test]
fn build_pio_env_joins_libs_with_platform_separator() {
    let libs = vec!["a".to_string(), "b".to_string()];
    let env = build_ci_pio_env(&libs, None);
    let expected = if cfg!(windows) { "a;b" } else { "a:b" };
    assert_eq!(
        env.get("PLATFORMIO_LIB_EXTRA_DIRS").map(String::as_str),
        Some(expected)
    );
    assert!(!env.contains_key("PLATFORMIO_PROJECT_CONFIG"));
}

#[test]
fn build_pio_env_omits_libs_key_when_empty() {
    let env = build_ci_pio_env(&[], None);
    assert!(env.is_empty());
}

#[test]
fn build_pio_env_falls_back_to_as_given_when_canonicalize_fails() {
    let bogus = "/this/path/does/not/exist/conf.ini";
    let env = build_ci_pio_env(&[], Some(bogus));
    assert_eq!(
        env.get("PLATFORMIO_PROJECT_CONFIG").map(String::as_str),
        Some(bogus)
    );
}

#[test]
fn ci_subcommand_round_trips_through_clap() {
    let argv = [
        "fbuild",
        "ci",
        "--board",
        "uno",
        "--lib",
        "./libs",
        "--lib",
        "./more",
        "-c",
        "custom.ini",
        "examples/Blink/Blink.ino",
    ];
    let cli = Cli::try_parse_from(argv).expect("parse");
    match cli.command {
        Some(Commands::Ci {
            board,
            libs,
            project_conf,
            sketches,
            ..
        }) => {
            assert_eq!(board, "uno");
            assert_eq!(libs, vec!["./libs".to_string(), "./more".to_string()]);
            assert_eq!(project_conf.as_deref(), Some("custom.ini"));
            assert_eq!(sketches, vec!["examples/Blink/Blink.ino".to_string()]);
        }
        _ => panic!("expected Ci subcommand"),
    }
}

#[test]
fn ci_short_board_flag_b_is_accepted() {
    let argv = ["fbuild", "ci", "-b", "uno", "examples/Blink"];
    let cli = Cli::try_parse_from(argv).expect("parse");
    match cli.command {
        Some(Commands::Ci {
            board, sketches, ..
        }) => {
            assert_eq!(board, "uno");
            assert_eq!(sketches, vec!["examples/Blink".to_string()]);
        }
        _ => panic!("expected Ci subcommand"),
    }
}

#[test]
fn ci_requires_at_least_one_sketch() {
    let argv = ["fbuild", "ci", "--board", "uno"];
    assert!(Cli::try_parse_from(argv).is_err());
}

#[test]
fn ci_quick_and_release_are_mutually_exclusive() {
    let argv = ["fbuild", "ci", "-b", "uno", "--quick", "--release", "."];
    assert!(Cli::try_parse_from(argv).is_err());
}

#[test]
fn compile_many_diag_stage2_flag_is_accepted() {
    let argv = [
        "fbuild",
        "compile-many",
        "--board",
        "uno",
        "--diag-stage2",
        "examples/Blink",
    ];
    let cli = Cli::try_parse_from(argv).expect("parse");
    match cli.command {
        Some(Commands::CompileMany { diag_stage2, .. }) => {
            assert!(diag_stage2);
        }
        _ => panic!("expected CompileMany subcommand"),
    }
}
