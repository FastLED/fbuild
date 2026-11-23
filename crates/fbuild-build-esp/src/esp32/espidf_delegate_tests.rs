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
            sdkconfig_defaults: None,
        }
    }

    /// The same request with a chip that has a silicon family.
    fn p4_request(
        &self,
        chip: &str,
        sdkconfig_defaults: Option<NormalizedPath>,
    ) -> IdfBuildRequest {
        IdfBuildRequest {
            project_dir: self.project(),
            build_dir: self.build(),
            chip: chip.to_string(),
            sdkconfig_defaults,
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
            "#!/bin/sh\nprintf '%s|%s\\n' \"$0\" \"$*\" >> \"{log}\"\nprintf '%s|%s|%s|%s\\n' \"$IDF_PATH\" \"$IDF_TOOLS_PATH\" \"$PATH\" \"$SDKCONFIG_DEFAULTS\" >> \"{env}\"\n{body}\n",
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
    assert_eq!(fields.len(), 4, "{environment}");
    assert_eq!(
        fields[3], "",
        "a chip with one silicon family must not set SDKCONFIG_DEFAULTS: {environment}"
    );
    assert_eq!(fields[0], fake.source().to_string(), "{environment}");
    assert_eq!(fields[1], fake.path().to_string(), "{environment}");
    let path_entries: Vec<NormalizedPath> = std::env::split_paths(fields[2])
        .map(NormalizedPath::new)
        .collect();
    assert_eq!(
        path_entries[0].as_path(),
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

/// A chip the pinned release has no verified profile for is an explicit
/// configuration error rather than a missing-compiler crash later.
#[test]
fn an_unsupported_chip_is_refused_by_name() {
    assert_eq!(
        super::profile_for_chip("esp32s3").unwrap(),
        IdfProfile::Esp32S3Only
    );
    for chip in ["esp32", "esp32h2", "esp8266"] {
        let error = super::profile_for_chip(chip).unwrap_err();
        assert!(error.to_string().contains(chip), "{error}");
        assert!(error.to_string().contains("#1971"), "{error}");
        // The message lists what *is* served, so the fix is readable.
        assert!(error.to_string().contains("esp32c5"), "{error}");
    }
}

/// The RISC-V chips get the RISC-V profile: the Xtensa compiler cannot build
/// them, and installing both for every build would cost 3.3 GB instead of the
/// one compiler the chip needs (FastLED/fbuild#1878).
#[test]
fn riscv_chips_resolve_the_riscv_profile() {
    for chip in ["esp32c3", "esp32c5", "esp32c6", "esp32p4"] {
        assert_eq!(
            super::profile_for_chip(chip).unwrap(),
            IdfProfile::Riscv32,
            "{chip}"
        );
    }
}

/// The ESP32-P4 silicon family comes from the board's own variant: `esp32p4`
/// is rev-3 silicon (min 3.0), `esp32p4_es` is pre-rev-3 (revs up to 1.99).
/// Either way one release serves both, so nothing is refused for the pin's sake
/// (FastLED/fbuild#2133).
#[test]
fn the_p4_silicon_family_comes_from_the_board_variant() {
    let rev3 = super::silicon_family("esp32p4", "esp32p4")
        .unwrap()
        .unwrap();
    assert_eq!(rev3.selector, "CONFIG_ESP32P4_REV_MIN_300");
    assert_eq!(rev3.lines, ["CONFIG_ESP32P4_REV_MIN_300=y"]);
    let pre = super::silicon_family("esp32p4", "esp32p4_es")
        .unwrap()
        .unwrap();
    assert_eq!(pre.selector, "CONFIG_ESP32P4_SELECTS_REV_LESS_V3");
    assert_eq!(
        pre.lines,
        [
            "CONFIG_ESP32P4_SELECTS_REV_LESS_V3=y",
            "CONFIG_ESP32P4_REV_MIN_0=y"
        ]
    );
    // Every chip with a single family in the pinned release stays untouched.
    for chip in ["esp32s3", "esp32c3", "esp32c5", "esp32c6"] {
        assert!(
            super::silicon_family(chip, chip).unwrap().is_none(),
            "{chip}"
        );
    }
}

/// A P4 board that names neither family is refused by name: guessing would
/// build for silicon the board may not have.
#[test]
fn an_unknown_p4_variant_is_refused() {
    let message = super::silicon_family("esp32p4", "esp32p4_es2")
        .unwrap_err()
        .to_string();
    assert!(message.contains("esp32p4_es2"), "{message}");
    assert!(message.contains("esp32p4_es"), "{message}");
    assert!(message.contains("FastLED/fbuild#2133"), "{message}");
}

/// The family defaults are written where `idf.py` is pointed, with the family
/// symbol and nothing else.
#[test]
fn the_silicon_defaults_file_carries_the_family_symbol() {
    let dir = tempfile::TempDir::new().unwrap();
    let build = NormalizedPath::new(dir.path());
    let path = super::write_silicon_defaults(&build, &super::P4_REV3).unwrap();
    assert_eq!(path, build.join("silicon-rev3.defaults"));
    let text = std::fs::read_to_string(path.as_path()).unwrap();
    let symbols: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim_start().starts_with('#') && !line.trim().is_empty())
        .collect();
    assert_eq!(symbols, ["CONFIG_ESP32P4_REV_MIN_300=y"], "{text}");
}

/// `SDKCONFIG_DEFAULTS` replaces IDF's own list, so the project's files are
/// appended after the family entry — and only the ones that exist, because IDF
/// aborts a configure that names a missing file.
#[test]
fn the_sdkconfig_defaults_list_puts_the_family_first() {
    let fake = FakeIdf::new();
    let family = fake.build().join("silicon-rev3.defaults");
    let list =
        super::sdkconfig_defaults_list(&fake.p4_request("esp32p4", Some(family.clone())), &family);
    assert_eq!(list, family.to_string(), "only the family entry exists");

    std::fs::write(
        fake.project().join("sdkconfig.defaults").as_path(),
        "CONFIG_LOG_DEFAULT_LEVEL_WARN=y\n",
    )
    .unwrap();
    std::fs::write(
        fake.project().join("sdkconfig.defaults.esp32p4").as_path(),
        "CONFIG_ESPTOOLPY_FLASHSIZE_4MB=y\n",
    )
    .unwrap();
    let list =
        super::sdkconfig_defaults_list(&fake.p4_request("esp32p4", Some(family.clone())), &family);
    let entries: Vec<&str> = list.split(';').collect();
    assert_eq!(entries.len(), 3, "{list}");
    assert_eq!(entries[0], family.to_string(), "{list}");
    assert!(entries[1].ends_with("sdkconfig.defaults"), "{list}");
    assert!(entries[2].ends_with("sdkconfig.defaults.esp32p4"), "{list}");
}

/// The wiring, not just the helper: a real spawn for an ESP32-P4 board sees
/// `SDKCONFIG_DEFAULTS` in its environment, family entry first.
#[tokio::test]
async fn the_child_gets_the_silicon_family_defaults() {
    if fbuild_core::platform::host::is_windows() {
        return;
    }
    let fake = FakeIdf::new();
    fake.install_driver(&format!(
        "echo 'fake idf.py: compiling'\n{}exit 0",
        write_artifacts_script(&fake.build())
    ));
    let family = super::write_silicon_defaults(&fake.build(), &super::P4_REV3).unwrap();
    std::fs::write(
        fake.project().join("sdkconfig.defaults").as_path(),
        "CONFIG_LOG_DEFAULT_LEVEL_WARN=y\n",
    )
    .unwrap();

    IdfDelegate::new(fake.installed())
        .run(&fake.p4_request("esp32p4", Some(family.clone())))
        .await
        .unwrap();

    let environment = fake.child_environment();
    let fields: Vec<&str> = environment.split('|').collect();
    assert_eq!(fields.len(), 4, "{environment}");
    let entries: Vec<&str> = fields[3].split(';').collect();
    assert_eq!(entries.len(), 2, "SDKCONFIG_DEFAULTS: {}", fields[3]);
    assert_eq!(entries[0], family.to_string(), "{}", fields[3]);
    assert!(entries[1].ends_with("sdkconfig.defaults"), "{}", fields[3]);
}

/// A project `sdkconfig` that selected the other family is refused by name: a
/// `sdkconfig` outranks every default, so building with it would silently
/// produce an image for the other silicon.
#[test]
fn a_sdkconfig_pinning_the_other_family_is_refused() {
    let fake = FakeIdf::new();
    // A pre-rev-3 sdkconfig on a rev-3 board.
    std::fs::write(
        fake.project().join("sdkconfig").as_path(),
        "CONFIG_IDF_TARGET=\"esp32p4\"\nCONFIG_ESP32P4_SELECTS_REV_LESS_V3=y\n",
    )
    .unwrap();
    let message = super::reject_conflicting_sdkconfig(&fake.project(), &super::P4_REV3)
        .unwrap_err()
        .to_string();
    assert!(message.contains("sdkconfig"), "{message}");
    assert!(
        message.contains("CONFIG_ESP32P4_SELECTS_REV_LESS_V3"),
        "{message}"
    );
    // The same file is fine on a pre-rev-3 board.
    super::reject_conflicting_sdkconfig(&fake.project(), &super::P4_PRE_REV3).unwrap();
}

/// The provisioning line states the profile's real footprint instead of the S3
/// figure for every chip: the RISC-V compiler is almost twice the Xtensa one.
#[test]
fn the_provisioning_line_names_the_profile_footprint() {
    let s3 = super::provisioning_line("esp32s3", IdfProfile::Esp32S3Only);
    assert!(s3.contains("esp32s3"), "{s3}");
    assert!(s3.contains("~1.3 GB of toolchains"), "{s3}");
    let riscv = super::provisioning_line("esp32c5", IdfProfile::Riscv32);
    assert!(riscv.contains("esp32c5"), "{riscv}");
    assert!(riscv.contains("~2.3 GB of toolchains"), "{riscv}");
}

/// The delegated build reports its caching mode, and the mode it reports is
/// the one that ran: no compiler wrapper, so the log line says so and never
/// reads as if objects had been reused (FastLED/fbuild#1974).
#[test]
fn the_delegated_build_reports_cold_only_caching() {
    let mode = IdfCacheMode::ColdOnly;
    assert_eq!(mode.label(), "cold-only");
    let line = mode.log_line();
    assert!(line.contains("caching: cold-only"), "{line}");
    assert!(line.contains("no object is served"), "{line}");
    assert!(line.contains("FastLED/fbuild#1974"), "{line}");
}
