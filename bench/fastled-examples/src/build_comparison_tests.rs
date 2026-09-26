use super::*;

#[test]
fn fbuild_benchmark_env_enables_phase_logging_and_restart_diagnostics() {
    let envs = tool_envs(ToolKind::Fbuild, Path::new("benchmark-output/perf.jsonl"));
    let envs = envs
        .into_iter()
        .map(|(key, value)| (key, value.to_string_lossy().into_owned()))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(envs.get("FBUILD_PERF_LOG").map(String::as_str), Some("1"));
    assert_eq!(
        envs.get("FBUILD_PERF_LOG_JSON").map(String::as_str),
        Some("benchmark-output/perf.jsonl")
    );
    assert_eq!(
        envs.get("RUST_LOG").map(String::as_str),
        Some("fbuild_cli=info")
    );
    assert!(tool_envs(ToolKind::Arduino, Path::new("unused")).is_empty());
}

fn sample_results() -> Vec<ToolResult> {
    vec![
        ToolResult {
            board: "uno".into(),
            board_name: "Arduino Uno".into(),
            tool: "arduino".into(),
            display_name: "Arduino CLI".into(),
            version: "arduino-cli 1.5.0".into(),
            cold_ms: 1200.0,
            warm_ms: 800.0,
            speedup: 1.5,
            cold_trials_ms: vec![1100.0, 1200.0, 1300.0],
            warm_trials_ms: vec![750.0, 800.0, 850.0],
            cold_phases_ms: BTreeMap::new(),
            cold_phase_trials: Vec::new(),
            daemon_restarts: 0,
        },
        ToolResult {
            board: "uno".into(),
            board_name: "Arduino Uno".into(),
            tool: "platformio".into(),
            display_name: "PlatformIO".into(),
            version: "PlatformIO Core 6.1.19".into(),
            cold_ms: 900.0,
            warm_ms: 300.0,
            speedup: 3.0,
            cold_trials_ms: vec![850.0, 900.0, 950.0],
            warm_trials_ms: vec![280.0, 300.0, 320.0],
            cold_phases_ms: BTreeMap::new(),
            cold_phase_trials: Vec::new(),
            daemon_restarts: 0,
        },
        ToolResult {
            board: "uno".into(),
            board_name: "Arduino Uno".into(),
            tool: "fbuild".into(),
            display_name: "fbuild".into(),
            version: "fbuild 0.1.0".into(),
            cold_ms: 600.0,
            warm_ms: 40.0,
            speedup: 15.0,
            cold_trials_ms: vec![580.0, 600.0, 620.0],
            warm_trials_ms: vec![38.0, 40.0, 42.0],
            cold_phases_ms: BTreeMap::from([("compile".to_string(), 400.0)]),
            cold_phase_trials: vec![BTreeMap::from([("compile".to_string(), 400.0)])],
            daemon_restarts: 0,
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
        raw_baseline_ms: Some(450.0),
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
            BOARDS[0],
            OsStr::new("arduino-cli"),
            OsStr::new("pio"),
            project,
            fbuild,
            arduino_build,
        ),
        vec![
            command("arduino-cli", &["cache", "clean"]),
            ColdCleanupStep::RemoveDir(NormalizedPath::new(arduino_build)),
        ]
    );
    assert_eq!(
        cold_cleanup_steps(
            ToolKind::PlatformIo,
            BOARDS[0],
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
            BOARDS[0],
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
fn esp32s3_cleanup_uses_its_environment() {
    let steps = cold_cleanup_steps(
        ToolKind::PlatformIo,
        BOARDS[1],
        OsStr::new("arduino-cli"),
        OsStr::new("pio"),
        Path::new("bench/blink"),
        Path::new("target/release/fbuild"),
        Path::new("benchmark-output/arduino-esp32s3"),
    );
    assert!(matches!(
        &steps[1],
        ColdCleanupStep::Command { args, .. }
            if args.windows(2).any(|pair| pair == os_args(&["--environment", "esp32s3"]))
    ));
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
fn svg_contains_uno_and_esp32s3_groups() {
    let mut results = sample_results();
    results.extend(sample_results().into_iter().map(|mut result| {
        result.board = "esp32s3".into();
        result.board_name = "ESP32-S3".into();
        result
    }));
    let svg = render_svg(&sample_metadata(), &results);
    assert!(svg.contains("Arduino Uno"), "{svg}");
    assert!(svg.contains("ESP32-S3"), "{svg}");
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
    assert_eq!(manifest["artifacts"]["latest"]["schema_version"], 2);
    let history = fs::read_to_string(temp.path().join("history.jsonl")).unwrap();
    assert_eq!(history.lines().count(), HISTORY_MAX_LINES);
    assert!(history.lines().last().unwrap().contains("0123456789abcdef"));
    let latest: Value =
        serde_json::from_str(&fs::read_to_string(temp.path().join("latest.json")).unwrap())
            .unwrap();
    assert_eq!(latest["schema_version"], 2);
    assert_eq!(latest["metadata"]["boards"][0]["name"], "Arduino Uno");
    assert_eq!(latest["metadata"]["boards"][1]["name"], "ESP32-S3");
    assert_eq!(
        latest["metadata"]["toolchain_pins"]["esp32s3"]["arduino_core"],
        "esp32:esp32@3.3.7"
    );
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

fn phases(pairs: &[(&str, f64)]) -> BTreeMap<String, f64> {
    pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

#[test]
fn parses_perf_jsonl_lines_after_offset() {
    let content = [
        r#"{"label":"avr-orchestrator","phases":{"compile":1.0},"total_ms":1.0,"unix_ms":1}"#,
        r#"{"label":"pipeline","phases":{"link":9.0},"total_ms":9.0,"unix_ms":2}"#,
        "not json",
        r#"{"label":"avr-orchestrator","phases":{"compile":250.5,"link":30.0},"total_ms":280.5,"unix_ms":3}"#,
    ]
    .join("\n");
    assert_eq!(parse_perf_lines(&content, 1).len(), 2);
    assert_eq!(
        perf_phases_after(&content, 1, &["avr-orchestrator"]),
        Some(phases(&[("compile", 250.5), ("link", 30.0)]))
    );
    // Both timers of one AVR build are merged (pipeline carries compile/link).
    assert_eq!(
        perf_phases_after(&content, 1, &["avr-orchestrator", "pipeline"]),
        Some(phases(&[("compile", 250.5), ("link", 39.0)]))
    );
    assert_eq!(perf_phases_after(&content, 4, &["avr-orchestrator"]), None);
    assert_eq!(perf_phases_after("", 0, &["avr-orchestrator"]), None);
    let missing = tempfile::tempdir().unwrap();
    assert_eq!(perf_line_count(&missing.path().join("absent.jsonl")), 0);
}

#[test]
fn phase_medians_skip_missing_phases() {
    let trials = vec![
        phases(&[("compile", 100.0), ("link", 10.0)]),
        phases(&[("compile", 300.0)]),
        phases(&[("compile", 200.0), ("link", 30.0)]),
    ];
    assert_eq!(
        phase_medians(&trials),
        phases(&[("compile", 200.0), ("link", 20.0)])
    );
    assert!(phase_medians(&[]).is_empty());
}

#[test]
fn strips_compiler_wrapper_prefix_and_redirects_output() {
    let out = Path::new("/tmp/raw/0.o");
    let argv = [
        "/home/u/.cargo/bin/zccache",
        "fbuild.exe",
        "avr-g++",
        "-c",
        "blink.cpp",
        "-o",
        "build/blink.o",
    ]
    .map(String::from);
    assert_eq!(
        rewrite_compile_argv(&argv, out),
        ["avr-g++", "-c", "blink.cpp", "-o", "/tmp/raw/0.o"].map(String::from)
    );
    let joined = ["avr-gcc", "-c", "x.c", "-obuild/x.o"].map(String::from);
    assert_eq!(
        rewrite_compile_argv(&joined, out),
        ["avr-gcc", "-c", "x.c", "-o/tmp/raw/0.o"].map(String::from)
    );
    let no_output = ["avr-gcc", "-c", "x.c"].map(String::from);
    assert_eq!(
        rewrite_compile_argv(&no_output, out),
        ["avr-gcc", "-c", "x.c", "-o", "/tmp/raw/0.o"].map(String::from)
    );
    assert_eq!(
        split_command(r#"zccache avr-g++ "-DNAME=\"a b\"" -o out.o"#),
        ["zccache", "avr-g++", "-DNAME=\"a b\"", "-o", "out.o"].map(String::from)
    );
}

#[test]
fn latest_payload_reports_overhead_and_platformio_ratio() {
    let latest = latest_payload(&sample_metadata(), &sample_results());
    assert_eq!(latest["raw_baseline_ms"], 450.0);
    assert_eq!(latest["fbuild_overhead_ms"], 150.0);
    assert_eq!(latest["fbuild_vs_platformio_cold"], 0.667);
    assert_eq!(latest["results"][2]["cold_phases_ms"]["compile"], 400.0);
    assert_eq!(
        latest["results"][2]["cold_phase_trials"][0]["compile"],
        400.0
    );
    assert!(
        latest["results"][0]["cold_phase_trials"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    let mut metadata = sample_metadata();
    metadata.raw_baseline_ms = None;
    let latest = latest_payload(&metadata, &sample_results());
    assert!(latest["raw_baseline_ms"].is_null());
    assert!(latest["fbuild_overhead_ms"].is_null());

    let temp = tempfile::tempdir().unwrap();
    let path = NormalizedPath::new(temp.path().join("history.jsonl"));
    write_history(path.clone(), &sample_metadata(), &sample_results()).unwrap();
    let line: Value = serde_json::from_str(fs::read_to_string(&path).unwrap().trim()).unwrap();
    assert_eq!(line["fbuild_overhead_ms"], 150.0);
    assert_eq!(line["fbuild_vs_platformio_cold"], 0.667);
}

#[test]
fn ratio_regression_uses_seven_day_median() {
    let now = parse_timestamp_unix_s("2026-07-22T12:00:00Z").unwrap();
    assert_eq!(now, 1_784_721_600);
    let day = 86_400;
    let history = vec![
        json!({"ts": format!("unix:{}", now - 10 * day), "fbuild_vs_platformio_cold": 5.0}),
        json!({"ts": format!("unix:{}", now - 8 * day), "fbuild_vs_platformio_cold": 5.0}),
        json!({"ts": format!("unix:{}", now - 3 * day), "fbuild_vs_platformio_cold": 0.6}),
        json!({"ts": "2026-07-20T12:00:00Z", "fbuild_vs_platformio_cold": 0.7}),
        json!({"ts": format!("unix:{}", now - day), "fbuild_vs_platformio_cold": 0.8}),
        json!({"ts": "garbage", "fbuild_vs_platformio_cold": 9.0}),
        json!({"ts": format!("unix:{}", now - day)}),
    ];
    assert_eq!(ratio_regressed(&history, now, 0.75), Some(0.7));
    assert_eq!(ratio_regressed(&history, now, 0.7), None);
    assert_eq!(ratio_regressed(&history, now, 0.5), None);
    assert_eq!(ratio_regressed(&[], now, 9.0), None);
}

#[test]
fn svg_shows_raw_floor_and_overhead() {
    let svg = render_svg(&sample_metadata(), &sample_results());
    assert!(
        svg.contains(
            "raw compiler floor: 450.0 ms | fbuild overhead: 150.0 ms | fbuild/PIO cold: 0.667"
        ),
        "{svg}"
    );
    let mut metadata = sample_metadata();
    metadata.raw_baseline_ms = None;
    let svg = render_svg(&metadata, &sample_results());
    assert!(!svg.contains("raw compiler floor"));
    assert!(svg.contains("fbuild/PIO cold: 0.667"));
    let html = render_html(&sample_metadata(), &sample_results());
    assert!(html.contains("fbuild cold phase breakdown"));
}

/// FastLED/fbuild#1467: the raw-compiler baseline must replay the real
/// toolchain commands, not the clangd-flavored database.
#[test]
fn find_compile_db_prefers_the_raw_toolchain_database() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path();
    let env_dir = fbuild_paths::get_project_build_root(project).join("uno/release");
    fs::create_dir_all(&env_dir).unwrap();
    fs::write(env_dir.join("compile_commands.json"), "[]").unwrap();
    assert_eq!(
        find_compile_db(project, "uno")
            .unwrap()
            .file_name()
            .unwrap(),
        "compile_commands.json",
        "older fbuild builds only wrote the clangd database"
    );

    fs::write(env_dir.join("compile_commands.raw.json"), "[]").unwrap();
    assert_eq!(
        find_compile_db(project, "uno")
            .unwrap()
            .file_name()
            .unwrap(),
        "compile_commands.raw.json"
    );

    // A quick-profile database sorts before `release` but must not be used:
    // the timed builds are `--release`.
    let quick_dir = fbuild_paths::get_project_build_root(project).join("uno/quick");
    fs::create_dir_all(&quick_dir).unwrap();
    fs::write(quick_dir.join("compile_commands.raw.json"), "[]").unwrap();
    assert_eq!(
        find_compile_db(project, "uno").unwrap().as_path(),
        env_dir.join("compile_commands.raw.json")
    );
}

#[test]
fn restart_notice_is_detected_in_a_command_stderr() {
    assert!(restarted_daemon(
        b"daemon binary updated, restarting... (sibling binary is 0.5s newer; ...)\n"
    ));
}

#[test]
fn clean_command_stderr_is_not_a_restart() {
    assert!(!restarted_daemon(b"build succeeded in 0.0s\n"));
    assert!(!restarted_daemon(b""));
}
