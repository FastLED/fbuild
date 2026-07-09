//! Tests for `strip_cache_wrapper`.

use crate::compile_database::strip_cache_wrapper;

#[test]
fn test_strip_sccache() {
    let args = vec![
        "sccache".to_string(),
        "/usr/bin/gcc".to_string(),
        "-c".to_string(),
    ];
    let stripped = strip_cache_wrapper(&args);
    assert_eq!(stripped[0], "/usr/bin/gcc");
    assert_eq!(stripped.len(), 2);
}

#[test]
fn test_strip_zccache() {
    let args = vec![
        "/path/to/zccache".to_string(),
        "/usr/bin/gcc".to_string(),
        "-c".to_string(),
    ];
    let stripped = strip_cache_wrapper(&args);
    assert_eq!(stripped[0], "/usr/bin/gcc");
}

/// Legacy `zccache.exe wrap <compiler>` command-line shape (pre-embedded
/// zccache, soldr <0.7.59). soldr 1.12.11+ embeds zccache into
/// soldr-daemon and the RUSTC_WRAPPER is now soldr / `zccache-soldr`
/// (soldr#977 / #980 L1 / #1081), so production no longer emits this
/// shape. Test retained as a backward-compat parse assertion: if a
/// legacy compile_commands.json fragment is fed through
/// `strip_cache_wrapper`, the wrapper + `wrap` subcommand must still
/// be stripped off. See FastLED/fbuild#855.
#[test]
fn test_strip_zccache_wrap_mode() {
    let args = vec![
        "C:\\tools\\zccache.exe".to_string(),
        "wrap".to_string(),
        "C:\\tc\\bin\\xtensa-esp32-elf-g++.exe".to_string(),
        "-c".to_string(),
    ];
    let stripped = strip_cache_wrapper(&args);
    assert_eq!(stripped[0], "C:\\tc\\bin\\xtensa-esp32-elf-g++.exe");
    assert_eq!(stripped[1], "-c");
}

#[test]
fn test_strip_ccache() {
    let args = vec![
        "ccache".to_string(),
        "/usr/bin/gcc".to_string(),
        "-c".to_string(),
    ];
    let stripped = strip_cache_wrapper(&args);
    assert_eq!(stripped[0], "/usr/bin/gcc");
}

#[test]
fn test_strip_no_wrapper() {
    let args = vec!["/usr/bin/gcc".to_string(), "-c".to_string()];
    let stripped = strip_cache_wrapper(&args);
    assert_eq!(stripped, args);
}

#[test]
fn test_strip_empty() {
    let args: Vec<String> = vec![];
    let stripped = strip_cache_wrapper(&args);
    assert!(stripped.is_empty());
}

#[test]
fn test_strip_cache_wrapper_windows_exe() {
    let args = vec![
        "C:\\Users\\user\\.cargo\\bin\\sccache.exe".to_string(),
        "C:\\tools\\gcc.exe".to_string(),
        "-c".to_string(),
    ];
    let stripped = strip_cache_wrapper(&args);
    assert_eq!(stripped[0], "C:\\tools\\gcc.exe");
    assert_eq!(stripped.len(), 2);
}

#[test]
fn test_strip_cache_wrapper_case_insensitive() {
    // Windows file systems are case-insensitive
    for name in &[
        "SCCACHE", "Sccache", "ZCCACHE", "Zccache", "CCACHE", "Ccache",
    ] {
        let args = vec![
            name.to_string(),
            "/usr/bin/gcc".to_string(),
            "-c".to_string(),
        ];
        let stripped = strip_cache_wrapper(&args);
        assert_eq!(
            stripped[0], "/usr/bin/gcc",
            "failed to strip cache wrapper: {}",
            name
        );
    }
}

#[test]
fn test_strip_cache_wrapper_single_element_wrapper() {
    // Only the wrapper, no actual compiler — should return as-is
    let args = vec!["sccache".to_string()];
    let stripped = strip_cache_wrapper(&args);
    assert_eq!(stripped, args);
}

#[test]
fn test_strip_cache_wrapper_not_a_wrapper() {
    // File named "sccache-stats" shouldn't be stripped
    let args = vec!["sccache-stats".to_string(), "/usr/bin/gcc".to_string()];
    let stripped = strip_cache_wrapper(&args);
    // file_stem of "sccache-stats" is "sccache-stats", not "sccache"
    assert_eq!(stripped.len(), 2);
    assert_eq!(stripped[0], "sccache-stats");
}
