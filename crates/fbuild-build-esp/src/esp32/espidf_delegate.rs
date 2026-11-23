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
use fbuild_toolchain::toolchain::{
    EspIdfInstall, IDF_RELEASE_VERSION, IdfProfile, InstalledEspIdf,
};

use super::espidf_artifacts::{IdfArtifacts, discover_and_publish};
use super::espidf_compile_db;
use super::orchestrator::helpers::{IdfCmakeProof, esp32_idf_cmake_stack_summary};

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

/// How the delegated build's compilations are cached.
///
/// A delegated build hands compilation to `idf.py`/CMake/Ninja, so the compiler
/// is spawned by ninja and never by fbuild's embedded zccache dispatcher: the
/// Arduino path's warm object reuse does not apply here (FastLED/fbuild#1974).
/// The measured decision is **cold-only**, and it is a type rather than a
/// comment so the *report* has to state it: a stack summary or an artifact
/// index that names a cache mode names this one, and wiring a compiler launcher
/// later has to change the value (and the tests that pin it) instead of
/// silently starting to claim cached objects the build did not have.
///
/// Measured on the spike project with the pinned release (1047 ninja actions,
/// 2026-11-10): cold 41.8 s, `fullclean` + build 34.0 s, no-op 1.2 s unwrapped;
/// with `-DCMAKE_C_COMPILER_LAUNCHER=zccache` cold 50.6 s and `fullclean` +
/// build 110.6 s at a 0.2% hit rate — a pessimization with no reuse, because
/// zccache keys on the absolute compile command that IDF rewrites per build
/// directory. Numbers and method: `docs/design/bare-idf-component-builds.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum IdfCacheMode {
    /// No compiler wrapper: every ninja action compiles. Ninja's own
    /// incremental state inside the delegated build directory is the only
    /// reuse the path has.
    ColdOnly,
}

impl IdfCacheMode {
    /// The spelling used by the stack summary and the artifact index.
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::ColdOnly => "cold-only",
        }
    }

    /// The build-log line, so a slow delegated build is explained rather than
    /// investigated: it names the mode and says nothing was served from cache.
    pub(super) fn log_line(self) -> String {
        format!(
            "idf.py caching: {} — compiler invocations are driven by ninja, so no object is \
             served from fbuild's zccache; the delegated build directory's own incremental state \
             is the only reuse (FastLED/fbuild#1974)",
            self.label()
        )
    }
}

/// The IDF profile that provisions `chip`, or an error naming what exists.
///
/// A chip is listed once a real project has been built for it with the pinned
/// release (`espidf_delegate_e2e_tests.rs`); the rest of the release's target
/// matrix stays refused rather than assumed, because a chip without its cross
/// compiler in the profile would fail much later and far less legibly.
fn profile_for_chip(chip: &str) -> Result<IdfProfile> {
    match chip {
        "esp32s3" => Ok(IdfProfile::Esp32S3Only),
        "esp32c3" | "esp32c5" | "esp32c6" | "esp32p4" => Ok(IdfProfile::Riscv32),
        other => Err(FbuildError::ConfigError(format!(
            "{BUILD_SYSTEM_KEY} = espidf is not available for MCU '{other}': the pinned ESP-IDF \
             {IDF_RELEASE_VERSION} is provisioned for esp32s3, esp32c3, esp32c5, esp32c6 and \
             esp32p4 only. Use `framework = arduino` for this board, or build it with \
             PlatformIO (FastLED/fbuild#1971, #1970)"
        ))),
    }
}

/// The ESP32-P4 silicon family a board asks for, and the Kconfig lines that
/// select it.
///
/// ESP-IDF v5.5.2+ carries both P4 families in one release and picks between
/// them at configure time: `ESP32P4_SELECTS_REV_LESS_V3` moves
/// `ESP32P4_REV_MAX_FULL` between 199 (pre-rev-3) and 399 (rev-3), and the
/// `ESP32P4_REV_MIN` choice is only legal on the matching side of that switch,
/// so an image built for the wrong family links, flashes and then fails in the
/// bootloader. The board's own `build.chip_variant` decides which family it is
/// (FastLED/fbuild#2133).
#[derive(Debug)]
pub(super) struct SiliconFamily {
    /// Name of the fbuild-owned defaults file in the delegated build root.
    pub(super) file_name: &'static str,
    /// Kconfig lines written into it, in file order.
    pub(super) lines: &'static [&'static str],
    /// The symbol that selects this family.
    pub(super) selector: &'static str,
    /// The symbol that selects the other family.
    pub(super) opposite: &'static str,
}

/// Rev-3 ESP32-P4 silicon (`chip_variant = esp32p4`): revisions 3.0 and up.
pub(super) const P4_REV3: SiliconFamily = SiliconFamily {
    file_name: "silicon-rev3.defaults",
    lines: &["CONFIG_ESP32P4_REV_MIN_300=y"],
    selector: "CONFIG_ESP32P4_REV_MIN_300",
    opposite: "CONFIG_ESP32P4_SELECTS_REV_LESS_V3",
};

/// Pre-rev-3 ESP32-P4 silicon (the `esp32p4_es` boards): revisions up to 1.99.
pub(super) const P4_PRE_REV3: SiliconFamily = SiliconFamily {
    file_name: "silicon-pre-rev3.defaults",
    lines: &[
        "CONFIG_ESP32P4_SELECTS_REV_LESS_V3=y",
        "CONFIG_ESP32P4_REV_MIN_0=y",
    ],
    selector: "CONFIG_ESP32P4_SELECTS_REV_LESS_V3",
    opposite: "CONFIG_ESP32P4_REV_MIN_300",
};

/// The silicon family `chip`/`sdk_variant` names, or `None` when the chip has a
/// single family in the pinned release.
///
/// An ESP32-P4 board that names neither family is an error rather than a guess:
/// the two families need different Kconfig, and defaulting silently would ship
/// an image for silicon the board may not have.
fn silicon_family(chip: &str, sdk_variant: &str) -> Result<Option<&'static SiliconFamily>> {
    if chip != "esp32p4" {
        return Ok(None);
    }
    match sdk_variant {
        "esp32p4" => Ok(Some(&P4_REV3)),
        "esp32p4_es" => Ok(Some(&P4_PRE_REV3)),
        other => Err(FbuildError::ConfigError(format!(
            "{BUILD_SYSTEM_KEY} = espidf cannot tell which ESP32-P4 silicon '{other}' is: the \
             board's `build.chip_variant` must be `esp32p4` (rev-3 silicon) or `esp32p4_es` \
             (pre-rev-3 silicon), because the two need different ESP-IDF Kconfig families and an \
             image built for the wrong one fails in the bootloader (FastLED/fbuild#2133)"
        ))),
    }
}

/// Refuse a project `sdkconfig` that already selected the other family.
///
/// A `sdkconfig` outranks the whole defaults list, so carrying one over from a
/// build for the other family would produce exactly the wrong-silicon image the
/// family defaults exist to prevent. The file is named in the error rather than
/// overridden.
fn reject_conflicting_sdkconfig(
    project_dir: &NormalizedPath,
    family: &SiliconFamily,
) -> Result<()> {
    let path = project_dir.join("sdkconfig");
    let Ok(text) = std::fs::read_to_string(path.as_path()) else {
        return Ok(());
    };
    let conflicting = format!("{}=y", family.opposite);
    if text.lines().any(|line| line.trim() == conflicting.as_str()) {
        return Err(FbuildError::ConfigError(format!(
            "{path} selects {}, but this board is {} silicon: {BUILD_SYSTEM_KEY} = espidf builds \
             one ESP32-P4 family per image, so a sdkconfig that pins the other family cannot be \
             used. Delete the file (or keep it as `sdkconfig.defaults` without that symbol) and \
             rebuild (FastLED/fbuild#2133)",
            family.opposite, family.selector
        )));
    }
    Ok(())
}

/// Write the family's defaults file into the delegated build root.
///
/// It goes into fbuild's build directory, not the project: it is derived data
/// for this build, and the project stays the user's.
fn write_silicon_defaults(
    build_dir: &NormalizedPath,
    family: &SiliconFamily,
) -> Result<NormalizedPath> {
    let path = build_dir.join(family.file_name);
    let mut text = String::from(
        "# Written by fbuild: the ESP32-P4 silicon family the board declares.\n\
         # Changing these symbols by hand builds an image for other silicon.\n",
    );
    for line in family.lines {
        text.push_str(line);
        text.push('\n');
    }
    std::fs::write(path.as_path(), text)
        .map_err(|error| FbuildError::Other(format!("writing {path}: {error}")))?;
    Ok(path)
}

/// The build-log line announcing the install, with the profile's own measured
/// footprint: the RISC-V compiler is almost twice the Xtensa one, so one
/// figure for every chip would understate it.
fn provisioning_line(chip: &str, profile: IdfProfile) -> String {
    let tools = profile.measured_installed_bytes() + profile.measured_build_tools_bytes();
    // Decimal gigabytes with one digit, the unit the design note quotes.
    let tenths = tools / 100_000_000;
    format!(
        "ESP-IDF delegated build: provisioning the pinned release for {chip} (first run installs \
         ~{}.{} GB of toolchains and build tools, plus the ~0.5 GB source tree and Python \
         environment)",
        tenths / 10,
        tenths % 10
    )
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
    /// IDF target name (`esp32s3`, `esp32c5`, ...).
    pub chip: String,
    /// fbuild's own `SDKCONFIG_DEFAULTS` entry, when the chip has more than one
    /// silicon family (ESP32-P4 rev-3 vs pre-rev-3).
    pub sdkconfig_defaults: Option<NormalizedPath>,
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
        let env = self.env(request)?;
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
    /// the profile's directories *prepended* to the host's, plus the
    /// `SDKCONFIG_DEFAULTS` list when the board needs a silicon family.
    fn env(&self, request: &IdfBuildRequest) -> Result<Vec<(String, String)>> {
        let mut env: Vec<(String, String)> = self
            .installed
            .env_vars()
            .into_iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        env.push(("PATH".to_string(), prefixed_path(&self.installed)?));
        if let Some(family_defaults) = &request.sdkconfig_defaults {
            env.push((
                "SDKCONFIG_DEFAULTS".to_string(),
                sdkconfig_defaults_list(request, family_defaults),
            ));
        }
        Ok(env)
    }
}

/// The `SDKCONFIG_DEFAULTS` value: fbuild's family entry first, then the
/// project's own files, in IDF's own precedence order.
///
/// Setting the variable replaces IDF's default list (`sdkconfig.defaults` plus
/// `sdkconfig.defaults.<target>`), so those are appended explicitly. IDF aborts
/// a configure that names a file which does not exist, hence the existence
/// checks, and every entry is absolute because IDF resolves a relative one
/// against its own working directory.
fn sdkconfig_defaults_list(request: &IdfBuildRequest, family_defaults: &NormalizedPath) -> String {
    let mut files = vec![family_defaults.to_string()];
    let project_defaults = request.project_dir.join("sdkconfig.defaults");
    if project_defaults.as_path().is_file() {
        files.push(project_defaults.to_string());
    }
    let target_defaults = request
        .project_dir
        .join(format!("sdkconfig.defaults.{}", request.chip));
    if target_defaults.as_path().is_file() {
        files.push(target_defaults.to_string());
    }
    files.join(";")
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
    let family = silicon_family(&chip, ctx.board.sdk_variant())?;
    if let Some(family) = family {
        reject_conflicting_sdkconfig(&NormalizedPath::new(&params.project_dir), family)?;
    }
    let build_dir = NormalizedPath::new(&ctx.build_dir).join("idf");
    std::fs::create_dir_all(build_dir.as_path()).map_err(|error| {
        FbuildError::Other(format!(
            "creating the delegated build root {build_dir}: {error}"
        ))
    })?;

    ctx.build_log.push(provisioning_line(&chip, profile));
    let install = EspIdfInstall::new(&params.project_dir, profile)?;
    let installed = install.install().await?;

    // #2133: one release carries both ESP32-P4 silicon families, so the board's
    // own variant decides which one this image is for.
    let sdkconfig_defaults = match family {
        Some(family) => {
            ctx.build_log.push(format!(
                "ESP32-P4 silicon family: {} (board variant {}, from {})",
                family.selector,
                ctx.board.sdk_variant(),
                family.file_name
            ));
            Some(write_silicon_defaults(&build_dir, family)?)
        }
        None => None,
    };
    let request = IdfBuildRequest {
        project_dir: NormalizedPath::new(&params.project_dir),
        build_dir: build_dir.clone(),
        chip: chip.clone(),
        sdkconfig_defaults,
    };
    let delegate = IdfDelegate::new(installed);
    let cache = IdfCacheMode::ColdOnly;
    ctx.build_log.push(cache.log_line());
    let mut outcome = delegate.run(&request).await?;

    // #1973: `idf.py`'s database is rooted in the delegated build directory and
    // the IDF checkout, and it is not the path fbuild documents. Re-root it,
    // merge it with fbuild's own entries and publish the result where every
    // other fbuild database lives — before the artifact index and the
    // `integration=idf-cmake` proof read it.
    let published = espidf_compile_db::publish(
        &request.build_dir,
        &NormalizedPath::new(&ctx.build_dir),
        &request.project_dir,
    )?;
    outcome.artifacts.compile_commands = published.path.clone();
    let digest: String = published.digest.chars().take(16).collect();
    ctx.build_log.push(format!(
        "compile database: {} entries ({} kept from fbuild, {} re-rooted from idf.py) at {} sha256:{digest}",
        published.entries,
        published.from_fbuild,
        published.from_delegated,
        published.path
    ));
    ctx.build_log.push(format!(
        "idf.py steps: set-target exit {:?} ({} bytes of output), build exit {:?} ({} bytes)",
        outcome.configure.exit_code,
        outcome.configure.stdout_bytes,
        outcome.build.exit_code,
        outcome.build.stdout_bytes
    ));

    // #1972: a delegated build may report `integration=idf-cmake`, but only
    // once its own artifacts prove it. `verified` fails the build rather than
    // letting the run continue with a weaker label, so a build that produced
    // no bin/ELF, no compile commands or no sdkconfig is never described as a
    // CMake/component build.
    let proof = IdfCmakeProof::verified(&outcome.artifacts, IDF_RELEASE_VERSION)?;
    ctx.build_log
        .push(esp32_idf_cmake_stack_summary(&proof, &chip, cache));

    let index = outcome
        .artifacts
        .write_index(&chip, &build_dir, cache.label())?;
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
