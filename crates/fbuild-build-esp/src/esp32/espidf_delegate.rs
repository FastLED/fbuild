//! Delegated ESP-IDF build — the opt-in `idf.py` path (FastLED/fbuild#1971).
//!
//! fbuild's ESP32 orchestrator compiles sketches against the Arduino core and
//! links precompiled ESP-IDF archives; it has no CMake and no `idf.py`, so a
//! genuine `idf_component_register()` project cannot be built by it
//! (FastLED/fbuild#1664, rejected by #1730). This module is the explicit
//! opt-in that changes that, and only that: a project that sets
//! [`BUILD_SYSTEM_KEY`] = `espidf` has its own `CMakeLists.txt` built by the
//! pinned ESP-IDF release from #1970, with its artifacts published under
//! `<build>/idf/`. No existing project changes behaviour — the switch defaults
//! to `native`.
//!
//! Three properties are deliberate:
//!
//! - **The child is supervised, not trusted.** `idf.py` is a Python driver over
//!   CMake/Ninja, so it is a subprocess with its own process tree. Each step
//!   runs under an explicit deadline through
//!   [`fbuild_core::subprocess::run_command_reporting`], which stops the tree
//!   and keeps both bounded streams (FastLED/fbuild#1867, #1908).
//! - **A failure is structured.** A dead, timed-out or wedged step becomes a
//!   [`DelegateFailure`] rendered into the build error with the stage, argv,
//!   working directory, exit code and the tail of both streams — never a bare
//!   exit code.
//! - **The environment is ours, not the host's.** `PATH` is the profile's
//!   entries *prepended* to the host `PATH` (`idf.py` and CMake shell out to
//!   `git`/`ln`), and `IDF_PATH`/`IDF_TOOLS_PATH` point into the fbuild cache
//!   tree, so nothing is read from a host `~/.espressif` install.

use std::time::Duration;

use fbuild_config::PlatformIOConfig;
use fbuild_core::path::NormalizedPath;
use fbuild_core::subprocess::{CommandReport, run_command_reporting};
use fbuild_core::{FbuildError, Result};
use fbuild_toolchain::toolchain::{EspIdfInstall, IdfProfile, InstalledEspIdf};

use super::espidf_artifacts::{IdfArtifacts, discover_and_publish};

/// `platformio.ini` key that opts an environment into the delegated build.
///
/// A project-level switch rather than an environment variable on purpose: the
/// choice changes what "built" means for that project, the install footprint
/// (~1.8 GB per release) and the caching story, so it belongs in the file that
/// is reviewed with the project.
pub(super) const BUILD_SYSTEM_KEY: &str = "board_build.build_system";

/// How an ESP32 environment is built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EspBuildSystem {
    /// The Arduino core path — the default, unchanged for every existing
    /// project.
    Native,
    /// The project's own `CMakeLists.txt`, built by the pinned ESP-IDF.
    EspIdf,
}

/// Read [`BUILD_SYSTEM_KEY`] for `env_name`.
///
/// An unknown value is a configuration error that names the valid ones: the
/// switch decides whether the Arduino path or a delegated CMake build runs, so
/// a typo must not silently pick one.
pub(super) fn build_system(config: &PlatformIOConfig, env_name: &str) -> Result<EspBuildSystem> {
    let env = config.get_env_config(env_name)?;
    let Some(raw) = env.get(BUILD_SYSTEM_KEY) else {
        return Ok(EspBuildSystem::Native);
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "native" | "arduino" => Ok(EspBuildSystem::Native),
        "espidf" | "esp-idf" | "idf" => Ok(EspBuildSystem::EspIdf),
        other => Err(FbuildError::ConfigError(format!(
            "{BUILD_SYSTEM_KEY} = {other} is unsupported for environment '{env_name}': use \
             `native` (the default, the Arduino core) or `espidf` to build the project's own \
             CMakeLists.txt with the pinned ESP-IDF release (FastLED/fbuild#1971)"
        ))),
    }
}

/// The IDF profile that provisions `chip`, or an error naming what exists.
///
/// The pinned release currently provisions one target (ESP32-S3) — widening it
/// is a release-matrix change (#1970's follow-up), not something this path can
/// paper over: the chip's cross compiler would simply be absent.
fn profile_for_chip(chip: &str) -> Result<IdfProfile> {
    match chip {
        "esp32s3" => Ok(IdfProfile::Esp32S3Only),
        other => Err(FbuildError::ConfigError(format!(
            "{BUILD_SYSTEM_KEY} = espidf is not available for MCU '{other}': the pinned ESP-IDF \
             release provisions esp32s3 only. Use `frameworks = arduino` for this board, or \
             build it with PlatformIO (FastLED/fbuild#1971, #1970)"
        ))),
    }
}

/// Per-step deadlines.
///
/// Measured on the reference host: configure 15.9 s, cold build 50.3 s
/// (FastLED/fbuild#1761). The budgets are two orders of magnitude larger
/// because a first run also compiles the whole IDF component tree on a slow
/// host, and a deadline that fires on a legitimate build is worse than a long
/// one: the deadline exists to bound a *wedged* child, not a slow one.
const DEFAULT_CONFIGURE_TIMEOUT: Duration = Duration::from_secs(600);
const DEFAULT_BUILD_TIMEOUT: Duration = Duration::from_secs(1800);

/// What to build.
#[derive(Debug, Clone)]
pub(super) struct IdfBuildRequest {
    pub project_dir: NormalizedPath,
    /// The delegated build root (`<build>/idf`).
    pub build_dir: NormalizedPath,
    /// IDF target name (`esp32s3`).
    pub chip: String,
}

/// The delegated build's result, with the streams of both steps.
#[derive(Debug)]
pub(super) struct IdfBuildOutcome {
    pub artifacts: IdfArtifacts,
    pub configure: CommandReport,
    pub build: CommandReport,
}

/// A supervised `idf.py` runner for one installed release.
pub(super) struct IdfDelegate {
    installed: InstalledEspIdf,
    /// The pinned interpreter `idf.py` runs under.
    program: NormalizedPath,
    /// `$IDF_PATH/tools/idf.py` of the installed release.
    idf_py: NormalizedPath,
    configure_timeout: Duration,
    build_timeout: Duration,
}

impl IdfDelegate {
    pub(super) fn new(installed: InstalledEspIdf) -> Self {
        let program = installed.python().python();
        let idf_py = installed.source_root().join("tools").join("idf.py");
        Self {
            installed,
            program,
            idf_py,
            configure_timeout: DEFAULT_CONFIGURE_TIMEOUT,
            build_timeout: DEFAULT_BUILD_TIMEOUT,
        }
    }

    /// Override the deadlines (tests drive the timeout path in under a second).
    #[cfg(test)]
    pub(super) fn with_timeouts(mut self, configure: Duration, build: Duration) -> Self {
        self.configure_timeout = configure;
        self.build_timeout = build;
        self
    }

    /// Configure the project for `chip`, then build it.
    pub(super) async fn run(&self, request: &IdfBuildRequest) -> Result<IdfBuildOutcome> {
        let chip = request.chip.as_str();
        let configure = self
            .step(
                request,
                &["set-target", chip],
                self.configure_timeout,
                "set-target",
            )
            .await?;
        let build = self
            .step(request, &["build"], self.build_timeout, "build")
            .await?;
        let artifacts = discover_and_publish(&request.build_dir, &request.project_dir)?;
        Ok(IdfBuildOutcome {
            artifacts,
            configure,
            build,
        })
    }

    /// Run one `idf.py` step and turn a bad one into a [`DelegateFailure`].
    async fn step(
        &self,
        request: &IdfBuildRequest,
        extra: &[&str],
        budget: Duration,
        stage: &'static str,
    ) -> Result<CommandReport> {
        let argv = self.argv(request, extra);
        let env = self.env()?;
        let env_refs: Vec<(&str, &str)> = env
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        let report = run_command_reporting(
            &argv_refs,
            Some(request.project_dir.as_path()),
            Some(&env_refs),
            budget,
        )
        .await?;
        if report.success() {
            return Ok(report);
        }
        Err(DelegateFailure::new(stage, argv, request, report, budget).into_error())
    }

    /// `python idf.py -C <project> -B <build> <extra...>`.
    ///
    /// `-C` and `-B` are explicit rather than relying on the working directory:
    /// the project directory is also the caller's cwd, but the build directory
    /// is fbuild's, and a user's stale `<project>/build` must never be reused.
    fn argv(&self, request: &IdfBuildRequest, extra: &[&str]) -> Vec<String> {
        let mut argv = vec![
            self.program.to_string(),
            self.idf_py.to_string(),
            "-C".to_string(),
            request.project_dir.to_string(),
            "-B".to_string(),
            request.build_dir.to_string(),
            "--no-hints".to_string(),
        ];
        argv.extend(extra.iter().map(|arg| (*arg).to_string()));
        argv
    }

    /// The child environment: IDF variables from the install, plus `PATH` with
    /// the profile's directories *prepended* to the host's.
    fn env(&self) -> Result<Vec<(String, String)>> {
        let mut env: Vec<(String, String)> = self
            .installed
            .env_vars()
            .into_iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        env.push(("PATH".to_string(), prefixed_path(&self.installed)?));
        Ok(env)
    }
}

/// The profile's `PATH` entries followed by the host `PATH`.
///
/// Prepending is not a preference. A tools-only `PATH` makes `idf.py`/CMake
/// fail on the first `ln`/`git` lookup (measured inside mbedtls: "Could not
/// create symbolic link", preceded by "Could NOT find Git"), because IDF's
/// CMake layer shells out to the host's shell utilities (FastLED/fbuild#1971).
fn prefixed_path(installed: &InstalledEspIdf) -> Result<String> {
    use std::ffi::OsString;

    let mut entries: Vec<OsString> = installed
        .path_dirs()
        .iter()
        .map(|dir| OsString::from(dir.as_path()))
        .collect();
    if let Some(host) = std::env::var_os("PATH") {
        entries.extend(std::env::split_paths(&host).map(OsString::from));
    }
    std::env::join_paths(entries)
        .map(|joined| joined.to_string_lossy().into_owned())
        .map_err(|error| {
            FbuildError::Other(format!("building the ESP-IDF child PATH failed: {error}"))
        })
}

/// Everything needed to report a delegated step that did not succeed.
///
/// It exists as a struct rather than a formatted string so the fields stay
/// addressable: the rendered message is what a user reads, and the fields are
/// what a later `/api` field or a follow-up test can assert on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DelegateFailure {
    pub stage: &'static str,
    pub argv: Vec<String>,
    pub cwd: NormalizedPath,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub budget: Duration,
    pub stdout: String,
    pub stderr: String,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

impl DelegateFailure {
    fn new(
        stage: &'static str,
        argv: Vec<String>,
        request: &IdfBuildRequest,
        report: CommandReport,
        budget: Duration,
    ) -> Self {
        Self {
            stage,
            argv,
            cwd: request.project_dir.clone(),
            exit_code: report.exit_code,
            timed_out: report.timed_out,
            budget,
            stdout: report.stdout,
            stderr: report.stderr,
            stdout_bytes: report.stdout_bytes,
            stderr_bytes: report.stderr_bytes,
            stdout_truncated: report.stdout_truncated,
            stderr_truncated: report.stderr_truncated,
        }
    }

    /// The headline: what failed, where, and why in one line.
    fn headline(&self) -> String {
        let outcome = match (self.timed_out, self.exit_code) {
            (true, _) => format!("timed out after {}s", self.budget.as_secs()),
            (false, Some(code)) => format!("exited with code {code}"),
            (false, None) => "was stopped without an exit code".to_string(),
        };
        format!(
            "delegated ESP-IDF build failed at `idf.py {}` ({outcome})",
            self.stage
        )
    }

    /// A labelled, bounded stream: the caller sees how much was written and
    /// that a tail was kept, so a truncated diagnosis cannot pass for a
    /// complete one.
    fn stream(&self, name: &str, text: &str, total: u64, truncated: bool) -> String {
        let kept = text.len() as u64;
        let note = if truncated {
            format!("last {kept} of {total} bytes kept")
        } else {
            format!("{total} bytes")
        };
        format!("--- {name} ({note}) ---\n{}", text.trim_end())
    }

    fn render(&self) -> String {
        let argv = self.argv.join(" ");
        [
            self.headline(),
            format!("  command: {argv}"),
            format!("  cwd: {}", self.cwd.display_slash()),
            self.stream(
                "stdout",
                &self.stdout,
                self.stdout_bytes,
                self.stdout_truncated,
            ),
            self.stream(
                "stderr",
                &self.stderr,
                self.stderr_bytes,
                self.stderr_truncated,
            ),
        ]
        .join("\n")
    }

    fn into_error(self) -> FbuildError {
        if self.timed_out {
            return FbuildError::Timeout(self.render());
        }
        FbuildError::CommandFailed {
            message: self.render(),
            exit_code: self.exit_code.unwrap_or(1),
        }
    }
}

/// Build an opted-in environment through the pinned ESP-IDF release.
///
/// `ctx` supplies the already-resolved board and build directory; nothing in
/// the Arduino path runs, so a component project never resolves the Arduino
/// core, its `.a` archives or its linker recipe.
pub(super) async fn delegate_build(
    params: &crate::BuildParams,
    mut ctx: crate::pipeline::BuildContext,
) -> Result<crate::BuildResult> {
    let started = std::time::Instant::now();
    let chip = ctx.board.mcu.clone();
    let profile = profile_for_chip(&chip)?;
    let build_dir = NormalizedPath::new(&ctx.build_dir).join("idf");
    std::fs::create_dir_all(build_dir.as_path()).map_err(|error| {
        FbuildError::Other(format!(
            "creating the delegated build root {build_dir}: {error}"
        ))
    })?;

    ctx.build_log.push(format!(
        "ESP-IDF delegated build: provisioning the pinned release for {chip} (first run downloads ~1.8 GB)"
    ));
    let install = EspIdfInstall::new(&params.project_dir, profile)?;
    let installed = install.install().await?;

    let request = IdfBuildRequest {
        project_dir: NormalizedPath::new(&params.project_dir),
        build_dir: build_dir.clone(),
        chip: chip.clone(),
    };
    let delegate = IdfDelegate::new(installed);
    let outcome = delegate.run(&request).await?;
    ctx.build_log.push(format!(
        "idf.py steps: set-target exit {:?} ({} bytes of output), build exit {:?} ({} bytes)",
        outcome.configure.exit_code,
        outcome.configure.stdout_bytes,
        outcome.build.exit_code,
        outcome.build.stdout_bytes
    ));

    let index = outcome.artifacts.write_index(&chip, &build_dir)?;
    let message = format!(
        "ESP-IDF delegated build ({chip}): {} app image, {} bytes of artifacts, index {index}",
        outcome.artifacts.project,
        outcome.artifacts.total_bytes()
    );
    ctx.build_log.push(message.clone());

    Ok(crate::BuildResult {
        success: true,
        firmware_path: Some(outcome.artifacts.app_bin.as_path().to_path_buf()),
        elf_path: Some(outcome.artifacts.elf.as_path().to_path_buf()),
        size_info: None,
        symbol_map: None,
        build_time_secs: started.elapsed().as_secs_f64(),
        message,
        compile_database_path: Some(outcome.artifacts.compile_commands.as_path().to_path_buf()),
        build_log: ctx.build_log,
    })
}

#[cfg(test)]
#[path = "espidf_delegate_tests.rs"]
mod espidf_delegate_tests;

#[cfg(test)]
#[path = "espidf_delegate_e2e_tests.rs"]
mod espidf_delegate_e2e_tests;
