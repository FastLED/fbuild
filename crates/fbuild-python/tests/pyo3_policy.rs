use std::{fs, ops::Range};

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

fn yaml_indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

fn yaml_mapping_entry(line: &str) -> Option<(&str, &str)> {
    let content = line.trim_start();
    if content.is_empty() || content.starts_with(['#', '-']) {
        return None;
    }
    let (key, value) = content.split_once(':')?;
    Some((key.trim(), value.trim()))
}

fn yaml_scalar(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(value)
}

fn yaml_significant(line: &str) -> bool {
    let content = line.trim_start();
    !content.is_empty() && !content.starts_with('#')
}

fn yaml_mapping_block_in_scope(
    lines: &[&str],
    mut scope: Range<usize>,
    mut entry_indent: usize,
    path: &[&str],
) -> Option<(Range<usize>, usize)> {
    for key in path {
        let entry = scope.clone().find(|&index| {
            yaml_indent(lines[index]) == entry_indent
                && yaml_mapping_entry(lines[index]) == Some((*key, ""))
        })?;
        let end = ((entry + 1)..scope.end)
            .find(|&index| {
                yaml_significant(lines[index]) && yaml_indent(lines[index]) <= entry_indent
            })
            .unwrap_or(scope.end);
        scope = (entry + 1)..end;
        entry_indent += 2;
    }

    Some((scope, entry_indent))
}

fn yaml_mapping_block(lines: &[&str], path: &[&str]) -> Option<(Range<usize>, usize)> {
    yaml_mapping_block_in_scope(lines, 0..lines.len(), 0, path)
}

fn yaml_mapping_value_in_scope<'a>(
    lines: &[&'a str],
    scope: Range<usize>,
    entry_indent: usize,
    path: &[&str],
) -> Option<&'a str> {
    let (key, parent_path) = path.split_last()?;
    let (scope, entry_indent) =
        yaml_mapping_block_in_scope(lines, scope, entry_indent, parent_path)?;
    scope
        .filter_map(|index| {
            (yaml_indent(lines[index]) == entry_indent)
                .then(|| yaml_mapping_entry(lines[index]))
                .flatten()
        })
        .find_map(|(candidate, value)| (candidate == *key).then(|| yaml_scalar(value)))
}

fn yaml_mapping_value<'a>(lines: &[&'a str], path: &[&str]) -> Option<&'a str> {
    yaml_mapping_value_in_scope(lines, 0..lines.len(), 0, path)
}

fn yaml_sequence_values<'a>(lines: &[&'a str], path: &[&str]) -> Option<Vec<&'a str>> {
    let (scope, item_indent) = yaml_mapping_block(lines, path)?;
    Some(
        scope
            .filter_map(|index| {
                (yaml_indent(lines[index]) == item_indent)
                    .then(|| lines[index].trim_start().strip_prefix("- "))
                    .flatten()
                    .map(yaml_scalar)
            })
            .collect(),
    )
}

fn yaml_step_mapping_value<'a>(
    lines: &[&'a str],
    step_name: &str,
    path: &[&str],
) -> Option<&'a str> {
    let (steps, item_indent) = yaml_mapping_block(lines, &["jobs", "build", "steps"])?;
    let step = steps.clone().find(|&index| {
        if yaml_indent(lines[index]) != item_indent {
            return false;
        }
        let Some(item) = lines[index].trim_start().strip_prefix("- ") else {
            return false;
        };
        yaml_mapping_entry(item)
            .is_some_and(|(key, value)| key == "name" && yaml_scalar(value) == step_name)
    })?;
    let step_end = ((step + 1)..steps.end)
        .find(|&index| yaml_significant(lines[index]) && yaml_indent(lines[index]) <= item_indent)
        .unwrap_or(steps.end);
    yaml_mapping_value_in_scope(lines, (step + 1)..step_end, item_indent + 2, path)
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
    let workflow_lines = workflow.lines().collect::<Vec<_>>();
    let release_workflow_lines = release_workflow.lines().collect::<Vec<_>>();

    assert_eq!(
        yaml_step_mapping_value(&workflow_lines, "Setup soldr", &["with", "version"]),
        Some("0.9.6"),
        "the setup-soldr step needs soldr >= 0.9.5 for catalogue-v2 Apple SDK assets"
    );
    assert_eq!(
        yaml_mapping_value(
            &workflow_lines,
            &["jobs", "build", "env", "SOLDR_TOOLCHAIN_ORIGIN"]
        ),
        Some("https://zackees.github.io/soldr-toolchain"),
        "Apple SDK prepare and build steps must share a job-scoped catalogue origin"
    );
    for target in [
        "x86_64_unknown_linux_musl",
        "aarch64_unknown_linux_musl",
        "x86_64_apple_darwin",
        "aarch64_apple_darwin",
    ] {
        let target_cflags = format!("CFLAGS_{target}");
        assert_eq!(
            yaml_mapping_value(
                &workflow_lines,
                &["jobs", "build", "env", target_cflags.as_str()]
            ),
            Some("-Wno-error=date-time"),
            "zig cross builds need a job-scoped mimalloc-pprof diagnostic override: {target_cflags}"
        );
    }
    for job_limit in ["CARGO_BUILD_JOBS", "SOLDR_JOBS"] {
        assert_eq!(
            yaml_mapping_value(&workflow_lines, &["jobs", "build", "env", job_limit]),
            Some("1"),
            "native release lanes need a job-scoped hosted-runner memory limit: {job_limit}"
        );
    }
    let release_paths = yaml_sequence_values(&release_workflow_lines, &["on", "push", "paths"])
        .expect("release workflow must define on.push.paths");
    for release_input in [
        ".github/workflows/release-auto.yml",
        ".github/workflows/template_native_build.yml",
    ] {
        assert!(
            release_paths.contains(&release_input),
            "release workflow fixes must retrigger an incomplete publication: {release_input}"
        );
    }
}
