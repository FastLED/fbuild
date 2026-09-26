//! Regression guard for FastLED/fbuild#1239: the workspace must resolve
//! exactly ONE `running-process` package identity.
//!
//! fbuild depends on `running-process` directly AND transitively through
//! the embedded zccache. `running-process` exports unmangled
//! `rp_*_public` native symbols, so two resolved identities (two
//! versions, two sources, or two revisions) would link two copies of
//! those symbols into fbuild. The dependency cascade discipline is:
//! fbuild's direct pin (a git `rev`, or an exact `=X.Y.Z` crates.io
//! version) must resolve to the same identity as the pin inside the zccache
//! fbuild embeds — this test fails the build when the pins drift apart.

use std::path::Path;

fn workspace_root() -> &'static Path {
    // crates/fbuild-core -> workspace root.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root above crates/fbuild-core")
}

/// Extract every `[[package]]` block for `name` from Cargo.lock, returning
/// each block's `(version, source)` pair.
fn locked_packages(lock: &str, name: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    for block in lock.split("[[package]]") {
        let mut pkg_name = None;
        let mut version = None;
        let mut source = None;
        for line in block.lines().map(str::trim) {
            if let Some(v) = line.strip_prefix("name = ") {
                pkg_name = Some(v.trim_matches('"').to_string());
            } else if let Some(v) = line.strip_prefix("version = ") {
                version = Some(v.trim_matches('"').to_string());
            } else if let Some(v) = line.strip_prefix("source = ") {
                source = Some(v.trim_matches('"').to_string());
            }
        }
        if pkg_name.as_deref() == Some(name) {
            found.push((version.unwrap_or_default(), source.unwrap_or_default()));
        }
    }
    found
}

/// How the workspace Cargo.toml pins a dependency.
#[derive(Debug)]
enum Pin {
    /// `git = "...", rev = "<sha>"`.
    GitRev(String),
    /// Registry `version = "=X.Y.Z"` (exact).
    ExactVersion(String),
}

/// Quoted value of `key = "..."` on `line`, if present.
fn quoted_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let marker = format!("{key} = \"");
    let start = line.find(&marker)? + marker.len();
    line[start..].split('"').next()
}

/// The pin recorded for `dep` in the workspace Cargo.toml.
fn workspace_pin(cargo_toml: &str, dep: &str) -> Pin {
    let line = cargo_toml
        .lines()
        .find(|l| l.trim_start().starts_with(&format!("{dep} = {{")))
        .unwrap_or_else(|| panic!("no `{dep}` dependency line in workspace Cargo.toml"));
    if let Some(rev) = quoted_value(line, "rev") {
        return Pin::GitRev(rev.to_string());
    }
    let version = quoted_value(line, "version")
        .and_then(|v| v.strip_prefix('='))
        .unwrap_or_else(|| panic!("`{dep}` must pin a git rev or an exact `=` version: {line}"));
    Pin::ExactVersion(version.to_string())
}

#[test]
fn exactly_one_running_process_identity_matching_the_workspace_pin() {
    let root = workspace_root();
    let lock = std::fs::read_to_string(root.join("Cargo.lock")).expect("read Cargo.lock");
    let cargo_toml =
        std::fs::read_to_string(root.join("Cargo.toml")).expect("read workspace Cargo.toml");

    let packages = locked_packages(&lock, "running-process");
    assert_eq!(
        packages.len(),
        1,
        "Cargo.lock must resolve exactly one `running-process` identity — \
         multiple identities link duplicate rp_*_public symbols \
         (FastLED/fbuild#1239). Resolved: {packages:?}"
    );

    let (version, source) = &packages[0];
    let pin = workspace_pin(&cargo_toml, "running-process");
    let matches = match &pin {
        Pin::GitRev(rev) => source.contains(rev.as_str()),
        Pin::ExactVersion(pinned) => source.starts_with("registry+") && version == pinned,
    };
    assert!(
        matches,
        "the locked running-process must be the workspace-pinned identity.\n  \
         locked:  {version} @ {source}\n  \
         pinned:  {pin:?}\n  \
         The direct pin and the zccache release's transitive pin have drifted — \
         re-run the FastLED/fbuild#1239 cascade (running-process -> zccache -> fbuild)."
    );
}
