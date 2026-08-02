//! Regression guard for FastLED/fbuild#1239: the workspace must resolve
//! exactly ONE `running-process` package identity.
//!
//! fbuild depends on `running-process` directly AND transitively through
//! the embedded zccache. `running-process` exports unmangled
//! `rp_*_public` native symbols, so two resolved identities (two
//! versions, two sources, or two revisions) would link two copies of
//! those symbols into fbuild. The dependency cascade discipline is:
//! fbuild's direct git pin must be byte-identical to the pin inside the
//! zccache release fbuild embeds — this test fails the build when the
//! pins drift apart.

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

/// The `rev = "..."` recorded for `dep` in the workspace Cargo.toml.
fn workspace_pin_rev(cargo_toml: &str, dep: &str) -> String {
    let line = cargo_toml
        .lines()
        .find(|l| l.trim_start().starts_with(&format!("{dep} = {{")))
        .unwrap_or_else(|| panic!("no `{dep}` dependency line in workspace Cargo.toml"));
    let rev_key = "rev = \"";
    let start = line
        .find(rev_key)
        .unwrap_or_else(|| panic!("`{dep}` line has no rev pin: {line}"))
        + rev_key.len();
    line[start..]
        .split('"')
        .next()
        .expect("terminated rev string")
        .to_string()
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
    let pinned_rev = workspace_pin_rev(&cargo_toml, "running-process");
    assert!(
        source.contains(&pinned_rev),
        "the locked running-process source must be the workspace-pinned rev.\n  \
         locked:  {version} @ {source}\n  \
         pinned:  {pinned_rev}\n  \
         The direct pin and the zccache release's transitive pin have drifted — \
         re-run the FastLED/fbuild#1239 cascade (running-process -> zccache -> fbuild)."
    );
}
