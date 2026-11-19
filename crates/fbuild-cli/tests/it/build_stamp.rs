//! The shipped `fbuild` binary still reports the checkout commit after the
//! stamp moved out of `fbuild-core` into the binaries (FastLED/fbuild#1742).

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(env!("CARGO_MANIFEST_DIR"))
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[test]
fn long_version_reports_the_checkout_commit() {
    let output = Command::new(env!("CARGO_BIN_EXE_fbuild"))
        .arg("--version")
        .env("FBUILD_NO_UPDATE_CHECK", "1")
        .output()
        .expect("run fbuild --version");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let expected = match git(&["rev-parse", "--short=12", "HEAD"]) {
        Some(commit) => format!("{}+git.{commit}", env!("CARGO_PKG_VERSION")),
        // A source tarball has no checkout to stamp.
        None => format!("{}+git.unknown", env!("CARGO_PKG_VERSION")),
    };
    assert!(
        stdout.contains(&expected),
        "expected {expected:?} in {stdout:?}"
    );
}
