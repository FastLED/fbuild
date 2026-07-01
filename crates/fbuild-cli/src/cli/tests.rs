//! Unit tests for CLI argument normalization and `fbuild ci` parsing.

use super::args::{Cli, Commands, DaemonAction};
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

#[tokio::test]
async fn build_pio_env_joins_libs_with_platform_separator() {
    let libs = vec!["a".to_string(), "b".to_string()];
    let env = build_ci_pio_env(&libs, None).await;
    let expected = if cfg!(windows) { "a;b" } else { "a:b" };
    assert_eq!(
        env.get("PLATFORMIO_LIB_EXTRA_DIRS").map(String::as_str),
        Some(expected)
    );
    assert!(!env.contains_key("PLATFORMIO_PROJECT_CONFIG"));
}

#[tokio::test]
async fn build_pio_env_omits_libs_key_when_empty() {
    let env = build_ci_pio_env(&[], None).await;
    assert!(env.is_empty());
}

#[tokio::test]
async fn build_pio_env_falls_back_to_as_given_when_canonicalize_fails() {
    let bogus = "/this/path/does/not/exist/conf.ini";
    let env = build_ci_pio_env(&[], Some(bogus)).await;
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
fn daemon_running_process_json_flag_is_accepted() {
    let argv = ["fbuild", "daemon", "running-process", "--json"];
    let cli = Cli::try_parse_from(argv).expect("parse");
    match cli.command {
        Some(Commands::Daemon {
            action: DaemonAction::RunningProcess { json },
        }) => assert!(json),
        _ => panic!("expected daemon running-process subcommand"),
    }
}

#[test]
fn daemon_servicedef_alias_uses_running_process_action() {
    let argv = ["fbuild", "daemon", "servicedef"];
    let cli = Cli::try_parse_from(argv).expect("parse");
    match cli.command {
        Some(Commands::Daemon {
            action: DaemonAction::RunningProcess { json },
        }) => assert!(!json),
        _ => panic!("expected daemon servicedef alias"),
    }
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

// `--shrink` / `--no-shrink` parsing (FastLED/fbuild#496, part of #493).
// The flag is plumbed onto the global Cli and every build-adjacent subcommand
// but is otherwise unused in Phase 1a — these tests just exercise clap's
// surface so a future refactor doesn't silently break parsing.

#[test]
fn build_accepts_shrink_safe() {
    let argv = ["fbuild", "build", "--shrink=safe", "tests/platform/uno"];
    let cli = Cli::try_parse_from(argv).expect("parse");
    match cli.command {
        Some(Commands::Build { shrink, .. }) => {
            assert_eq!(
                shrink,
                Some(super::args::CliShrinkMode::Safe),
                "build --shrink=safe should parse to Safe",
            );
        }
        _ => panic!("expected Build subcommand"),
    }
}

#[test]
fn build_bare_shrink_defaults_to_auto() {
    let argv = ["fbuild", "build", "--shrink", "tests/platform/uno"];
    let cli = Cli::try_parse_from(argv).expect("parse");
    match cli.command {
        Some(Commands::Build { shrink, .. }) => {
            assert_eq!(shrink, Some(super::args::CliShrinkMode::Auto));
        }
        _ => panic!("expected Build subcommand"),
    }
}

#[test]
fn build_no_shrink_flag_is_accepted() {
    let argv = ["fbuild", "build", "--no-shrink", "tests/platform/uno"];
    let cli = Cli::try_parse_from(argv).expect("parse");
    match cli.command {
        Some(Commands::Build {
            shrink, no_shrink, ..
        }) => {
            assert_eq!(shrink, None);
            assert!(no_shrink);
        }
        _ => panic!("expected Build subcommand"),
    }
}

#[test]
fn shrink_and_no_shrink_together_is_rejected() {
    // clap's `conflicts_with` should turn this into a parse error.
    let argv = [
        "fbuild",
        "build",
        "--shrink=safe",
        "--no-shrink",
        "tests/platform/uno",
    ];
    assert!(
        Cli::try_parse_from(argv).is_err(),
        "--shrink and --no-shrink must conflict",
    );
}

#[test]
fn build_accepts_all_shrink_modes() {
    for mode in ["auto", "off", "safe", "aggressive", "printf"] {
        let arg = format!("--shrink={mode}");
        let argv = ["fbuild", "build", &arg, "tests/platform/uno"];
        let cli = Cli::try_parse_from(argv)
            .unwrap_or_else(|e| panic!("--shrink={mode} should parse but got: {e}"));
        assert!(matches!(cli.command, Some(Commands::Build { .. })));
    }
}

#[test]
fn global_shrink_flag_is_accepted() {
    let argv = ["fbuild", "--shrink=safe", "build", "tests/platform/uno"];
    let cli = Cli::try_parse_from(argv).expect("parse");
    assert_eq!(cli.shrink, Some(super::args::CliShrinkMode::Safe));
}
