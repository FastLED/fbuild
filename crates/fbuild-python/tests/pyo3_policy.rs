use std::fs;

use fbuild_core::path::NormalizedPath;

fn repo_root() -> NormalizedPath {
    let manifest_dir = NormalizedPath::from(env!("CARGO_MANIFEST_DIR"));
    NormalizedPath::new(
        manifest_dir
            .as_path()
            .parent()
            .and_then(|path| path.parent())
            .expect("fbuild-python must remain under crates/"),
    )
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

    for command in [
        "soldr build --release --target ${{ inputs.target }} \\",
        "PYO3_NO_PYTHON=1 soldr build --release \\",
    ] {
        assert!(
            workflow.contains(command),
            "Windows MSVC cross-build lost the blessed soldr entry point: {command}"
        );
    }

    assert!(
        !workflow
            .lines()
            .any(|line| line.trim_start().starts_with("cargo xwin build")),
        "Windows MSVC commands must go through soldr build, not cargo-xwin directly"
    );

    let release_workflow =
        fs::read_to_string(root.join(".github/workflows/release-auto.yml")).unwrap();
    for target in ["x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc"] {
        assert!(
            release_workflow.contains(target),
            "release matrix lost required Windows MSVC target: {target}"
        );
    }
}
