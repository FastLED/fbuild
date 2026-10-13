//! Public Windows repair entrypoint: discoverable and isolated from builds.

use std::process::Command;

#[test]
fn help_documents_fix_win_without_starting_a_daemon() {
    // allow-direct-spawn: compiled CLI acceptance test.
    let output = Command::new(env!("CARGO_BIN_EXE_fbuild"))
        .arg("--help")
        .env("CI", "true")
        .output()
        .expect("run help");
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("--fix-win"), "missing repair flag: {help}");
    assert!(
        help.contains("long paths"),
        "missing repair purpose: {help}"
    );
}

#[test]
fn fix_win_on_non_windows_exits_without_building_or_prompting() {
    if fbuild_core::platform::host::is_windows() {
        return;
    }
    let root = tempfile::tempdir().expect("isolated home");
    // allow-direct-spawn: compiled CLI acceptance test.
    let output = Command::new(env!("CARGO_BIN_EXE_fbuild"))
        .args(["--fix-win", "--no-update-check"])
        .current_dir(root.path())
        .env("HOME", root.path())
        .env("CI", "true")
        .output()
        .expect("run Windows repair flag");
    assert!(!output.status.success());
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    assert!(diagnostic.contains("Windows"), "wrong error: {diagnostic}");
    assert!(!root.path().join(fbuild_paths::FBUILD_DIR_NAME).exists());
    assert!(output.stdout.is_empty(), "repair emitted build output");
}
