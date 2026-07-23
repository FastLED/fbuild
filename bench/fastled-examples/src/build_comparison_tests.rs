use super::*;

fn sample_results() -> Vec<ToolResult> {
    vec![
        ToolResult {
            tool: "arduino".into(),
            display_name: "Arduino CLI".into(),
            version: "arduino-cli 1.5.0".into(),
            cold_ms: 1200.0,
            warm_ms: 800.0,
            speedup: 1.5,
            cold_trials_ms: vec![1100.0, 1200.0, 1300.0],
            warm_trials_ms: vec![750.0, 800.0, 850.0],
        },
        ToolResult {
            tool: "platformio".into(),
            display_name: "PlatformIO".into(),
            version: "PlatformIO Core 6.1.19".into(),
            cold_ms: 900.0,
            warm_ms: 300.0,
            speedup: 3.0,
            cold_trials_ms: vec![850.0, 900.0, 950.0],
            warm_trials_ms: vec![280.0, 300.0, 320.0],
        },
        ToolResult {
            tool: "fbuild".into(),
            display_name: "fbuild".into(),
            version: "fbuild 0.1.0".into(),
            cold_ms: 600.0,
            warm_ms: 40.0,
            speedup: 15.0,
            cold_trials_ms: vec![580.0, 600.0, 620.0],
            warm_trials_ms: vec![38.0, 40.0, 42.0],
        },
    ]
}

fn sample_metadata() -> Metadata {
    Metadata {
        generated_at: "2026-07-22T12:00:00Z".into(),
        git_sha: "0123456789abcdef".into(),
        repository: DEFAULT_REPOSITORY.into(),
        run_url: "https://github.com/FastLED/fbuild/actions/runs/1".into(),
        project: "bench/blink".into(),
        trials: 3,
    }
}

fn command(program: &str, args: &[&str]) -> ColdCleanupStep {
    ColdCleanupStep::Command {
        program: OsString::from(program),
        args: os_args(args),
    }
}

#[test]
fn median_handles_odd_and_even_trial_counts() {
    assert_eq!(median(&[9.0, 1.0, 5.0]), 5.0);
    assert_eq!(median(&[9.0, 1.0, 7.0, 3.0]), 5.0);
}

#[test]
fn remove_dir_within_guards_boundaries() {
    let sandbox = tempfile::tempdir().unwrap();
    let root = sandbox.path().join("root");
    let nested = root.join("nested");
    let sibling = sandbox.path().join("sibling");
    fs::create_dir_all(&nested).unwrap();
    fs::create_dir_all(&sibling).unwrap();

    assert!(remove_dir_within(&root, &root).is_err());
    assert!(remove_dir_within(&root, &sibling).is_err());
    assert!(root.is_dir());
    assert!(sibling.is_dir());

    remove_dir_within(&root, &nested).unwrap();
    assert!(!nested.exists());
}

#[test]
fn every_trial_prepares_cold_once_and_never_prepares_warm() {
    assert_eq!(
        measurement_plan(3),
        vec![
            MeasurementStep::PrepareCold(1),
            MeasurementStep::ColdBuild(1),
            MeasurementStep::WarmBuild(1),
            MeasurementStep::PrepareCold(2),
            MeasurementStep::ColdBuild(2),
            MeasurementStep::WarmBuild(2),
            MeasurementStep::PrepareCold(3),
            MeasurementStep::ColdBuild(3),
            MeasurementStep::WarmBuild(3),
        ]
    );
}

#[test]
fn each_tool_has_the_complete_cold_cleanup_sequence() {
    let project = Path::new("bench/blink");
    let arduino_build = Path::new("benchmark-output/arduino-build");
    let fbuild = Path::new("target/release/fbuild");

    assert_eq!(
        cold_cleanup_steps(
            ToolKind::Arduino,
            OsStr::new("arduino-cli"),
            OsStr::new("pio"),
            project,
            fbuild,
            arduino_build,
        ),
        vec![
            command("arduino-cli", &["cache", "clean"]),
            ColdCleanupStep::RemoveDir(arduino_build.to_path_buf()),
        ]
    );
    assert_eq!(
        cold_cleanup_steps(
            ToolKind::PlatformIo,
            OsStr::new("arduino-cli"),
            OsStr::new("pio"),
            project,
            fbuild,
            arduino_build,
        ),
        vec![
            command("pio", &["system", "prune", "--cache", "--force"]),
            command(
                "pio",
                &[
                    "run",
                    "--project-dir",
                    "bench/blink",
                    "--environment",
                    "uno",
                    "--target",
                    "clean",
                ],
            ),
        ]
    );
    assert_eq!(
        cold_cleanup_steps(
            ToolKind::Fbuild,
            OsStr::new("arduino-cli"),
            OsStr::new("pio"),
            project,
            fbuild,
            arduino_build,
        ),
        vec![command(
            "target/release/fbuild",
            &[
                "clean",
                "cache",
                "bench/blink",
                "--environment",
                "uno",
                "--release",
            ],
        )]
    );
}

#[test]
fn svg_uses_reference_palette_and_warm_overlay() {
    let svg = render_svg(&sample_metadata(), &sample_results());
    for color in [
        "#3b4046", "#8b949e", "#1f3a7a", "#79c0ff", "#5b1f1c", "#f85149",
    ] {
        assert!(svg.contains(color), "missing {color}");
    }
    assert!(svg.contains("height=\"28\""));
    assert!(svg.contains("height=\"14\""));
    assert!(svg.contains("cold (back) + warm (front overlay)"));
}

#[test]
fn outputs_include_agent_discovery_and_bounded_history() {
    let temp = tempfile::tempdir().unwrap();
    let history = (0..HISTORY_MAX_LINES)
        .map(|index| format!(r#"{{"old":{index}}}"#))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(temp.path().join("history.jsonl"), history).unwrap();
    write_outputs(
        temp.path(),
        &sample_metadata(),
        &sample_results(),
        DEFAULT_PAGES_URL,
        DEFAULT_RAW_BASE_URL,
    )
    .unwrap();

    for file in [
        "manifest.json",
        "latest.json",
        "history.jsonl",
        "benchmark.svg",
        "index.html",
        ".nojekyll",
    ] {
        assert!(temp.path().join(file).is_file(), "missing {file}");
    }
    let manifest: Value =
        serde_json::from_str(&fs::read_to_string(temp.path().join("manifest.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["branch"], "benchmark-stats");
    assert_eq!(
        manifest["artifacts"]["history"]["max_lines"],
        HISTORY_MAX_LINES
    );
    let history = fs::read_to_string(temp.path().join("history.jsonl")).unwrap();
    assert_eq!(history.lines().count(), HISTORY_MAX_LINES);
    assert!(history.lines().last().unwrap().contains("0123456789abcdef"));
    let latest: Value =
        serde_json::from_str(&fs::read_to_string(temp.path().join("latest.json")).unwrap())
            .unwrap();
    assert_eq!(
        latest["metadata"]["cold_definition"],
        "project outputs, reusable framework objects, compiler-object caches, and Arduino/PlatformIO download/HTTP caches removed; installed packages/toolchains and fbuild package archives retained"
    );
    let html = fs::read_to_string(temp.path().join("index.html")).unwrap();
    assert!(html.contains("stable discovery index for agents"));
    assert!(html.contains("compiler-object caches"));
    assert!(html.contains("Arduino/PlatformIO download/HTTP caches"));
    assert!(html.contains("fbuild package archives"));
}
