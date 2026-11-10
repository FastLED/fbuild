//! Whole-release install of the pinned ESP-IDF (FastLED/fbuild#1970).
//!
//! The three fbuild packages a delegated `idf.py` build (#1971) needs are pinned
//! separately — tool archives ([`super::esp_idf_release`]), the source tree
//! ([`super::esp_idf_source`]) and the Python environment
//! ([`super::esp_idf_python`]) — because they are independent downloads with
//! independent cache keys. This module is the one place that installs all three
//! and answers the only question a build orchestrator should ask: *what
//! environment do I hand the subprocess?*
//!
//! Two properties matter and are both tested here:
//!
//! - **Ordering is part of the answer.** `idf.py` is a Python program whose
//!   shebang is the pinned venv interpreter, and `idf.py` in turn re-executes
//!   from `IDF_PATH`. So the venv's `bin/` must come *before* the tool `bin/`
//!   directories on `PATH`; otherwise a host `python3` wins and ESP-IDF's own
//!   dependency checker rejects an environment that is in fact correct.
//! - **No `~/.espressif`-shaped side tree exists.** Tool roots are exported as
//!   `PATH` entries plus the two variables CMake reads (`ESP_ROM_ELF_DIR`,
//!   `OPENOCD_SCRIPTS`). `IDF_TOOLS_PATH` is still set, but to the python
//!   entry's `python_env/` — the directory holding this release's constraints
//!   file, which `idf_tools.py` refuses to run without. It is a location, not a
//!   tool tree; see [`InstalledEspIdf::env_vars`].
//!
//! Nothing here builds anything. It is the input to #1971.

use std::collections::HashSet;
use std::path::Path;

use fbuild_core::Result;
use fbuild_core::path::NormalizedPath;

use super::esp_idf_python::{EspIdfPython, IdfPythonEnvironment};
use super::esp_idf_release::{IdfComponent, IdfProfile};
use super::esp_idf_release_profile::{EspIdfRelease, IdfEnvironment, dir_bytes};
use super::esp_idf_source::EspIdfSource;

/// The three fbuild packages that make up one ESP-IDF release.
///
/// A unit is listed separately from a component because the units fail
/// independently and are cached independently: a re-run after a failed Python
/// install must not re-download the 450 MB source tree, and this enum is what a
/// future `--report` would name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum IdfInstallUnit {
    /// The pinned source checkout (`esp-idf-v5.5.1/source`).
    Source,
    /// Every tool archive the profile installs.
    Tools,
    /// The pinned interpreter and its locked requirements.
    Python,
}

impl IdfInstallUnit {
    /// Every unit, in install order.
    pub const ALL: [Self; 3] = [Self::Source, Self::Tools, Self::Python];

    /// Cache key / log name of the unit.
    pub const fn id(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Tools => "tools",
            Self::Python => "python",
        }
    }
}

/// A fully installed release: the tree, the tools and the interpreter.
#[derive(Debug, Clone)]
pub struct InstalledEspIdf {
    source_root: NormalizedPath,
    tools: IdfEnvironment,
    python: IdfPythonEnvironment,
}

impl InstalledEspIdf {
    /// Assemble from already-installed pieces (used by tests and by callers that
    /// resolve roots themselves, e.g. from a caller-supplied cache root).
    pub fn from_parts(
        source_root: NormalizedPath,
        tools: IdfEnvironment,
        python: IdfPythonEnvironment,
    ) -> Self {
        Self {
            source_root,
            tools,
            python,
        }
    }

    /// Root of the pinned IDF source tree (`IDF_PATH`).
    pub fn source_root(&self) -> &NormalizedPath {
        &self.source_root
    }

    /// The tool roots of the installed profile.
    pub fn tools(&self) -> &IdfEnvironment {
        &self.tools
    }

    /// The Python environment `idf.py` must run in.
    pub fn python(&self) -> &IdfPythonEnvironment {
        &self.python
    }

    /// `PATH` entries, most specific first.
    ///
    /// The venv's `bin` leads: `idf.py`'s shebang resolves `python` from here,
    /// and ESP-IDF's startup check rejects the environment if a host interpreter
    /// answers instead. Tool `bin/` directories follow in install order, which
    /// keeps `xtensa-esp32-elf-gcc` (the profile's cross compiler) ahead of any
    /// other tool that ships a same-named script.
    pub fn path_dirs(&self) -> Vec<NormalizedPath> {
        let mut dirs = vec![self.python.bin_dir()];
        dirs.extend(self.tools.path_dirs());
        dirs
    }

    /// Environment variables for a delegated build, in a stable order.
    ///
    /// `IDF_PATH` is exported rather than passed as `--idf-path` because
    /// `idf.py` re-invokes `idf_tools.py` and CMake from child processes, and
    /// children inherit the environment — not the parent's argv.
    ///
    /// `IDF_TOOLS_PATH` **is** set, and that is deliberate: not to
    /// `~/.espressif`, but to the python entry's `python_env/`, the directory
    /// holding this release's `espidf.constraints.v5.5.txt`.
    /// `idf_tools.py::get_constraints(online=False)` looks for that file under
    /// `IDF_TOOLS_PATH` and calls `fatal()` when it is missing, so without this
    /// entry `idf.py --version` never gets as far as printing a version. The
    /// variable is a *location*, not a side tree: no tool is installed through
    /// it, `IDF_PYTHON_ENV_PATH` governs the venv, and every tool is on `PATH`
    /// because [`Self::path_dirs`] put it there.
    pub fn env_vars(&self) -> Vec<(&'static str, NormalizedPath)> {
        let mut vars = vec![
            ("IDF_PATH", self.source_root.clone()),
            ("IDF_PYTHON_ENV_PATH", self.python.env_dir().clone()),
            ("IDF_TOOLS_PATH", self.python.python_env_dir()),
        ];
        vars.extend(self.tools.env_vars());
        vars
    }

    /// Real bytes on disk of all three packages, `du`-style: symlinks are not
    /// followed and a hardlinked file is counted once.
    ///
    /// One identity set spans the whole release because a future component may
    /// hardlink an interpreter or a tool into another entry; counting the
    /// entries separately would then over-report exactly like the per-component
    /// walk did before #1976.
    pub fn installed_bytes(&self) -> u64 {
        let mut seen: HashSet<fbuild_core::platform::fs::FileIdentity> = HashSet::new();
        let source = dir_bytes(self.source_root.as_path(), &mut seen);
        let tools = self
            .tools
            .tool_roots()
            .iter()
            .map(|(_, root)| dir_bytes(root.as_path(), &mut seen))
            .sum::<u64>();
        let python = dir_bytes(self.python.env_dir().as_path(), &mut seen);
        source + tools + python
    }

    /// The units that are present on disk.
    ///
    /// Reported separately from [`Self::installed_bytes`] so a partial tree can
    /// be named in an error instead of showing up as a suspiciously small byte
    /// count.
    pub fn missing_units(&self) -> Vec<IdfInstallUnit> {
        let mut missing = Vec::new();
        if !self.source_root.as_path().is_dir() {
            missing.push(IdfInstallUnit::Source);
        }
        if self.tools.tool_roots().is_empty() {
            missing.push(IdfInstallUnit::Tools);
        }
        if !self.python.python().as_path().is_file() {
            missing.push(IdfInstallUnit::Python);
        }
        missing
    }
}

/// Installs one pinned ESP-IDF release: source + tools + Python.
pub struct EspIdfInstall {
    profile: IdfProfile,
    source: EspIdfSource,
    release: EspIdfRelease,
    python: EspIdfPython,
}

impl EspIdfInstall {
    /// Resolve every package against the default cache.
    pub fn new(project_dir: &Path, profile: IdfProfile) -> Result<Self> {
        Ok(Self {
            profile,
            source: EspIdfSource::new(project_dir)?,
            release: EspIdfRelease::new(project_dir, profile)?,
            python: EspIdfPython::new(project_dir)?,
        })
    }

    /// Install into an explicit cache root (tests and the end-to-end proof).
    ///
    /// `#[cfg(test)]` on purpose, matching the sibling modules: an explicit root
    /// is how the proof keeps ~1.6 GB of installed trees out of the user's real
    /// cache.
    #[cfg(test)]
    pub(in crate::toolchain) fn with_cache_root(
        project_dir: &Path,
        cache_root: &Path,
        profile: IdfProfile,
    ) -> Result<Self> {
        Ok(Self {
            profile,
            source: EspIdfSource::with_cache_root(project_dir, cache_root)?,
            release: EspIdfRelease::with_cache_root(project_dir, cache_root, profile)?,
            python: EspIdfPython::with_cache_root(project_dir, cache_root)?,
        })
    }

    /// The profile this installer provisions.
    pub fn profile(&self) -> IdfProfile {
        self.profile
    }

    /// True only when all three units are installed.
    ///
    /// Deliberately not `source && tools.is_installed() && python`: each of those
    /// is stricter about *its* markers than a build needs, and this method is
    /// the cheap pre-flight; [`Self::install`] is what repairs the difference.
    pub fn is_installed(&self) -> bool {
        self.source.is_installed() && self.release.is_installed() && self.python.is_installed()
    }

    /// Install the three units in order and return the build environment.
    ///
    /// Order is `source -> tools -> python`, and it is the cheap-to-diagnose
    /// order rather than the dependency order (none of the three depends on
    /// another): the source tree is the one whose absence breaks the most
    /// later steps, and the Python environment is the one whose install can
    /// still fail after everything else succeeded (`pip` against PyPI).
    ///
    /// A unit already installed is skipped by its own package, so a second call
    /// is a no-op and a failed call is repaired on re-run without re-downloading
    /// what already landed.
    pub async fn install(&self) -> Result<InstalledEspIdf> {
        let source_root = self.source.ensure_normalized().await?;
        let tools = self.release.install().await?;
        let python = self.python.install().await?;
        Ok(InstalledEspIdf::from_parts(source_root, tools, python))
    }

    /// Real bytes of the three packages currently on disk, `du`-style.
    ///
    /// Each package is its own cache entry, so each is measured with its own
    /// identity set; the units share no files by construction (separate keys
    /// under `esp-idf-v5.5.1/`).
    pub fn installed_bytes(&self) -> u64 {
        let mut source_seen: HashSet<fbuild_core::platform::fs::FileIdentity> = HashSet::new();
        let source = dir_bytes(self.source.install_path().as_path(), &mut source_seen);
        source + self.release.installed_bytes() + self.python.installed_bytes()
    }

    /// The tool components the profile installs (re-exported for reporting).
    pub fn components(&self) -> &'static [IdfComponent] {
        self.profile.components()
    }
}

#[cfg(test)]
#[path = "esp_idf_install_tests.rs"]
mod esp_idf_install_tests;
