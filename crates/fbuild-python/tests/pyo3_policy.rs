use std::fs;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("fbuild-python must remain under crates/")
        .to_path_buf()
}

#[test]
fn pyo3_029_policy_stays_target_python_independent() {
    // FastLED/fbuild#1025: keep every cross-build branch explicit until
    // fbuild adopts a soldr release with automatic PyO3 policy.
    let root = repo_root();
    let workspace_manifest = fs::read_to_string(root.join("Cargo.toml")).unwrap();
    let crate_manifest = fs::read_to_string(root.join("crates/fbuild-python/Cargo.toml")).unwrap();
    let workflow =
        fs::read_to_string(root.join(".github/workflows/template_native_build.yml")).unwrap();

    assert!(
        workspace_manifest.contains("pyo3 = { version = \"0.29\", features = [\"abi3-py310\"] }")
    );
    assert!(crate_manifest.contains("pyo3-build-config = \"0.29\""));
    assert!(crate_manifest
        .contains("pyo3-async-runtimes = { version = \"0.29\", features = [\"tokio-runtime\"] }"));

    for removed in [
        "PYO3_CROSS_LIB_DIR",
        "PYO3_CROSS_PYTHON_VERSION",
        "PYO3_CROSS_PYTHON_IMPLEMENTATION",
        "python3.lib",
        "www.nuget.org",
    ] {
        assert!(
            !workflow.contains(removed),
            "retired target-Python workaround returned: {removed}"
        );
    }

    for command in [
        "PYO3_NO_PYTHON=1 soldr cargo zigbuild --release \\",
        "PYO3_NO_PYTHON=1 soldr build --release \\",
        "PYO3_NO_PYTHON=1 cargo zigbuild --release \\",
        "PYO3_NO_PYTHON=1 soldr cargo build --release \\",
    ] {
        assert!(
            workflow.contains(command),
            "cross-build branch lost host-interpreter suppression: {command}"
        );
    }
}
