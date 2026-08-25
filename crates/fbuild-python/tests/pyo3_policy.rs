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
    assert!(
        crate_manifest.contains(
            "pyo3-async-runtimes = { version = \"0.29\", features = [\"tokio-runtime\"] }"
        )
    );

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
        "PYO3_NO_PYTHON=1 soldr --no-cache build --release \\",
        "PYO3_NO_PYTHON=1 cargo zigbuild --release \\",
        "PYO3_NO_PYTHON=1 soldr cargo build --release \\",
    ] {
        assert!(
            workflow.contains(command),
            "cross-build branch lost host-interpreter suppression: {command}"
        );
    }

    // The Windows MSVC branches route through `soldr --no-cache build`
    // (the xwin CRT-casing fixes made the cache bypass part of the
    // blessed invocation); the policy is the soldr entry point plus
    // host-interpreter suppression, not the exact cache flags.
    for command in [
        "soldr --no-cache build --release --target ${{ inputs.target }} \\",
        "PYO3_NO_PYTHON=1 soldr --no-cache build --release \\",
    ] {
        assert!(
            workflow.contains(command),
            "Windows MSVC cross-build lost the blessed soldr entry point: {command}"
        );
    }

    assert!(
        !workflow.lines().any(|line| {
            line.split_whitespace()
                .collect::<Vec<_>>()
                .windows(3)
                .any(|tokens| tokens == ["cargo", "xwin", "build"])
        }),
        "Windows MSVC commands must go through soldr build, not cargo-xwin directly"
    );

    let release_workflow =
        fs::read_to_string(root.join(".github/workflows/release-auto.yml")).unwrap();
    for target in ["x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc"] {
        assert!(
            release_workflow
                .lines()
                .any(|line| line.trim() == format!("- target: {target}")),
            "release matrix lost required Windows MSVC target: {target}"
        );
    }
}

#[test]
fn native_release_workflow_uses_current_cross_toolchains() {
    let root = repo_root();
    let workflow =
        fs::read_to_string(root.join(".github/workflows/template_native_build.yml")).unwrap();
    let release_workflow =
        fs::read_to_string(root.join(".github/workflows/release-auto.yml")).unwrap();

    assert!(
        workflow.contains("version: 0.9.6"),
        "native release builds need soldr >= 0.9.5 for catalogue-v2 Apple SDK assets"
    );
    assert!(
        workflow.contains("SOLDR_TOOLCHAIN_ORIGIN: https://zackees.github.io/soldr-toolchain"),
        "Apple SDK prepare and build steps must share the catalogue origin"
    );
    assert!(
        workflow.contains("CFLAGS=\"-Wno-error=date-time\" cargo zigbuild --release --target"),
        "musl release builds must demote zig's date-time error for mimalloc-pprof"
    );
    for job_limit in ["CARGO_BUILD_JOBS: \"2\"", "SOLDR_JOBS: \"2\""] {
        assert!(
            workflow.contains(job_limit),
            "native release lanes must stay below hosted-runner memory limits: {job_limit}"
        );
    }
    for release_input in [
        "- .github/workflows/release-auto.yml",
        "- .github/workflows/template_native_build.yml",
    ] {
        assert!(
            release_workflow.contains(release_input),
            "release workflow fixes must retrigger an incomplete publication: {release_input}"
        );
    }
}
