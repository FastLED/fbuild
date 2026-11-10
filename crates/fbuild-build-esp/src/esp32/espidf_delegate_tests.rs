//! Unit tests for the delegated ESP-IDF build (FastLED/fbuild#1971).
//!
//! The spawn tests drive the REAL spawn path against a fake `idf.py`: the fake
//! is the venv interpreter the install would have provisioned
//! (`.../bin/python`), so argv, cwd, environment and artifact discovery are all
//! exercised exactly as in production. The fake is a POSIX shell script, so
//! those cells return early on Windows — the same shape `fbuild-deploy`'s
//! containment tests use. The opt-in parsing needs no spawn and runs
//! everywhere.

use std::time::Duration;

use fbuild_core::path::NormalizedPath;
use fbuild_toolchain::toolchain::{IdfEnvironment, IdfPythonEnvironment, InstalledEspIdf};

use super::*;

/// Write an executable script without exec'ing a file still open for writing.
fn write_executable(path: &NormalizedPath, body: &str) {
    let parent = path.as_path().parent().unwrap();
    std::fs::create_dir_all(parent).unwrap();
    let staging = path.as_path().with_extension("staging");
    std::fs::write(&staging, body).unwrap();
    fbuild_core::platform::fs::set_executable(&staging).unwrap();
    std::fs::rename(&staging, path.as_path()).unwrap();
}

/// A fake installed release: a source tree with `tools/idf.py`, a venv whose
/// `bin/python` is the fake driver, and a project directory.
struct FakeIdf {
    dir: tempfile::TempDir,
}

impl FakeIdf {
    fn new() -> Self {
        let dir = tempfile::TempDir::new().unwrap();
        let fake = Self { dir };
        std::fs::create_dir_all(fake.source().join("tools").as_path()).unwrap();
        std::fs::write(
            fake.source().join("tools").join("idf.py").as_path(),
            "# fake\n",
        )
        .unwrap();
        std::fs::create_dir_all(fake.project().as_path()).unwrap();
        std::fs::create_dir_all(fake.build().as_path()).unwrap();
        fake
    }

    fn path(&self) -> NormalizedPath {
        NormalizedPath::new(self.dir.path())
    }

    fn source(&self) -> NormalizedPath {
        self.path().join("idf")
    }

    fn venv(&self) -> NormalizedPath {
        self.path().join("venv")
    }

    fn project(&self) -> NormalizedPath {
        self.path().join("project")
    }

    fn build(&self) -> NormalizedPath {
        self.path().join("build-idf")
    }

    fn log(&self) -> NormalizedPath {
        self.path().join("argv.log")
    }

    fn env_log(&self) -> NormalizedPath {
        self.path().join("env.log")
    }

    fn python(&self) -> NormalizedPath {
        self.venv().join("bin").join("python")
    }

    fn installed(&self) -> InstalledEspIdf {
        InstalledEspIdf::from_parts(
            self.source(),
            IdfEnvironment::from_roots(Vec::new()),
            IdfPythonEnvironment::new(self.venv()),
        )
    }

    fn request(&self) -> IdfBuildRequest {
        IdfBuildRequest {
            project_dir: self.project(),
            build_dir: self.build(),
            chip: "esp32s3".to_string(),
        }
    }

    /// One entry per invocation, each the space-joined argv the fake received.
    fn invocations(&self) -> Vec<(String, Vec<String>)> {
        std::fs::read_to_string(self.log().as_path())
            .unwrap_or_default()
            .lines()
            .map(|line| {
                let (program, args) = line.split_once('|').unwrap_or((line, ""));
                (
                    program.to_string(),
                    args.split(' ').map(str::to_string).collect(),
                )
            })
            .collect()
    }

    /// `IDF_PATH|IDF_TOOLS_PATH|PATH` as the fake child saw it (last run).
    fn child_environment(&self) -> String {
        std::fs::read_to_string(self.env_log().as_path())
            .unwrap_or_default()
            .lines()
            .last()
            .unwrap_or_default()
            .to_string()
    }

    /// Install the fake driver and return the script it runs.
    fn install_driver(&self, body: &str) -> NormalizedPath {
        // One line per invocation: `$0` is the program the delegate spawned,
        // `$*` is the argv it received (the interpreter path is argv[0] of the
        // child, so it does not appear in `$*`).
        let script = format!(
            "#!/bin/sh\nprintf '%s|%s\\n' \"$0\" \"$*\" >> \"{log}\"\nprintf '%s|%s|%s\\n' \"$IDF_PATH\" \"$IDF_TOOLS_PATH\" \"$PATH\" >> \"{env}\"\n{body}\n",
            log = self.log().display_slash(),
            env = self.env_log().display_slash(),
            body = body,
        );
        write_executable(&self.python(), &script);
        self.python()
    }
}

/// The artifact writes a successful fake build performs, reproducing the
/// layout `idf.py` leaves behind.
fn write_artifacts_script(build: &NormalizedPath) -> String {
    let build = build.display_slash();
    format!(
        r#"mkdir -p "{build}/bootloader" "{build}/partition_table"
printf 'bin' > "{build}/component_demo.bin"
printf 'elf' > "{build}/component_demo.elf"
printf 'map' > "{build}/component_demo.map"
printf 'boot' > "{build}/bootloader/bootloader.bin"
printf 'part' > "{build}/partition_table/partition-table.bin"
printf 'sdk' > "{build}/sdkconfig"
printf '[]' > "{build}/compile_commands.json"
printf '%s' '{{"flash_files": {{"0x0": "bootloader/bootloader.bin"}}, "app": {{"offset": "0x10000", "file": "component_demo.bin"}}, "bootloader": {{"offset": "0x0", "file": "bootloader/bootloader.bin"}}, "partition-table": {{"offset": "0x8000", "file": "partition_table/partition-table.bin"}}}}' > "{build}/flasher_args.json"
"#
    )
}

/// `set-target` then `build` run against the installed environment, and the
/// published artifacts are the ones IDF's own metadata named.
#[tokio::test]
async fn set_target_then_build_runs_with_the_installed_environment() {
    if fbuild_core::platform::host::is_windows() {
        return;
    }
    let fake = FakeIdf::new();
    fake.install_driver(&format!(
        "echo 'fake idf.py: compiling'\n{}exit 0",
        write_artifacts_script(&fake.build())
    ));

    let outcome = IdfDelegate::new(fake.installed())
        .run(&fake.request())
        .await
        .unwrap();

    let invocations = fake.invocations();
    assert_eq!(invocations.len(), 2, "{invocations:?}");

    // The child that ran is the venv interpreter the install provisioned, and
    // its first argument is the installed `idf.py`.
    let (program, argv) = &invocations[0];
    assert_eq!(*program, fake.python().to_string(), "{invocations:?}");
    assert_eq!(
        argv[0],
        fake.source().join("tools").join("idf.py").to_string(),
        "argv: {argv:?}"
    );
    assert_eq!(argv[1], "-C");
    assert_eq!(argv[2], fake.project().to_string());
    assert_eq!(argv[3], "-B");
    assert_eq!(argv[4], fake.build().to_string());
    assert_eq!(
        argv[5..],
        ["--no-hints", "set-target", "esp32s3"],
        "argv: {argv:?}"
    );
    assert_eq!(invocations[1].0, *program);
    assert_eq!(
        invocations[1].1[5..],
        ["--no-hints", "build"],
        "{invocations:?}"
    );

    // The child environment is ours: IDF_PATH/IDF_TOOLS_PATH point into the
    // install, and PATH leads with the venv's bin directory but still carries
    // the host's entries (idf.py shells out to git/ln).
    let environment = fake.child_environment();
    let fields: Vec<&str> = environment.split('|').collect();
    assert_eq!(fields.len(), 3, "{environment}");
    assert_eq!(fields[0], fake.source().to_string(), "{environment}");
    assert_eq!(fields[1], fake.path().to_string(), "{environment}");
    let path_entries: Vec<std::path::PathBuf> = std::env::split_paths(fields[2]).collect();
    assert_eq!(
        path_entries[0],
        fake.venv().join("bin").as_path(),
        "{environment}"
    );
    assert!(
        path_entries.len() > 1,
        "the host PATH was dropped: {environment}"
    );

    assert_eq!(outcome.artifacts.project, "component_demo");
    assert_eq!(
        outcome.artifacts.bootloader_bin.as_path(),
        fake.build().join("bootloader.bin").as_path()
    );
    assert!(outcome.configure.stdout.contains("fake idf.py"));
    assert!(outcome.build.success());
}

/// A step that exits non-zero becomes a structured failure: stage, argv, exit
/// code and both bounded streams, not a bare code.
#[tokio::test]
async fn a_failed_step_reports_stage_exit_code_and_both_streams() {
    if fbuild_core::platform::host::is_windows() {
        return;
    }
    let fake = FakeIdf::new();
    let marker = fake.path().join("configured");
    fake.install_driver(&format!(
        "if [ -f \"{marker}\" ]; then\n\
           echo 'configure done, compiling'\n\
           echo 'CMake Error: simulated failure' >&2\n\
           exit 3\n\
         fi\n\
         touch \"{marker}\"\n\
         exit 0",
        marker = marker.display_slash()
    ));

    let error = IdfDelegate::new(fake.installed())
        .run(&fake.request())
        .await
        .expect_err("a failed build must not report success");
    let message = error.to_string();
    assert!(
        message.contains("delegated ESP-IDF build failed at `idf.py build`"),
        "{message}"
    );
    assert!(message.contains("exited with code 3"), "{message}");
    assert!(
        message.contains("CMake Error: simulated failure"),
        "{message}"
    );
    assert!(message.contains("configure done, compiling"), "{message}");
    assert!(message.contains("--- stdout"), "{message}");
    assert!(message.contains("--- stderr"), "{message}");
    assert!(
        message.contains(&fake.build().display_slash()),
        "the failing argv must name the build dir: {message}"
    );
    match error {
        fbuild_core::FbuildError::CommandFailed { exit_code, .. } => assert_eq!(exit_code, 3),
        other => panic!("expected a structured command failure, got {other:?}"),
    }
}

/// A wedged step is bounded: the child is stopped, the result is a timeout, and
/// what it printed before the deadline survives into the message.
#[tokio::test]
async fn a_wedged_step_times_out_with_both_streams() {
    if fbuild_core::platform::host::is_windows() {
        return;
    }
    let fake = FakeIdf::new();
    let marker = fake.path().join("configured");
    fake.install_driver(&format!(
        "if [ -f \"{marker}\" ]; then\n\
           echo 'compiling, then wedged'\n\
           echo 'ninja: still running' >&2\n\
           sleep 30\n\
           exit 0\n\
         fi\n\
         touch \"{marker}\"\n\
         exit 0",
        marker = marker.display_slash()
    ));

    let started = std::time::Instant::now();
    let error = IdfDelegate::new(fake.installed())
        .with_timeouts(Duration::from_secs(30), Duration::from_millis(500))
        .run(&fake.request())
        .await
        .expect_err("a wedged build must not report success");
    let elapsed = started.elapsed();

    let message = error.to_string();
    assert!(
        matches!(error, fbuild_core::FbuildError::Timeout(_)),
        "{message}"
    );
    assert!(message.contains("timed out after"), "{message}");
    assert!(message.contains("compiling, then wedged"), "{message}");
    assert!(message.contains("ninja: still running"), "{message}");
    assert!(
        elapsed < Duration::from_secs(20),
        "the deadline did not bound the child: {elapsed:?}"
    );
}

/// `board_build.build_system` defaults to the native path, accepts `espidf`,
/// and refuses an unknown value by naming the valid ones.
#[test]
fn build_system_switch_defaults_to_native_and_rejects_typos() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config_for = |body: &str| {
        std::fs::write(tmp.path().join("platformio.ini"), body).unwrap();
        fbuild_config::PlatformIOConfig::from_path(&tmp.path().join("platformio.ini")).unwrap()
    };

    let config = config_for(
        "[env:esp32s3]\nplatform = espressif32\nboard = esp32-s3-devkitc-1\nframework = espidf\n",
    );
    assert_eq!(
        build_system(&config, "esp32s3").unwrap(),
        EspBuildSystem::Native
    );

    let config = config_for(
        "[env:esp32s3]\nplatform = espressif32\nboard = esp32-s3-devkitc-1\nframework = espidf\n\
         board_build.build_system = espidf\n",
    );
    assert_eq!(
        build_system(&config, "esp32s3").unwrap(),
        EspBuildSystem::EspIdf
    );

    let config = config_for(
        "[env:esp32s3]\nplatform = espressif32\nboard = esp32-s3-devkitc-1\nframework = espidf\n\
         board_build.build_system = cmake\n",
    );
    let error = build_system(&config, "esp32s3").unwrap_err();
    assert!(error.to_string().contains("`espidf`"), "{error}");
    assert!(error.to_string().contains("`native`"), "{error}");
}

/// The pinned release provisions one target; another chip is an explicit
/// configuration error rather than a missing-compiler crash later.
#[test]
fn an_unsupported_chip_is_refused_by_name() {
    super::profile_for_chip("esp32s3").unwrap();
    let error = super::profile_for_chip("esp32c6").unwrap_err();
    assert!(error.to_string().contains("esp32c6"), "{error}");
    assert!(error.to_string().contains("#1971"), "{error}");
}
