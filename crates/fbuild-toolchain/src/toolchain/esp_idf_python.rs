//! The Python environment the pinned ESP-IDF release needs (FastLED/fbuild#1970).
//!
//! The tool components ([`super::esp_idf_release`]) and the source tree
//! ([`super::esp_idf_source`]) are only two of the three inputs a delegated
//! `idf.py` build (#1971) needs. The third is the environment `idf.py` itself
//! runs in: ESP-IDF refuses to start unless its own dependency checker
//! (`tools/check_python_dependencies.py`) is satisfied by the interpreter it was
//! launched with.
//!
//! ## Why `install.sh` is not the answer
//!
//! Measured 2026-11-09 on this NixOS host: `install.sh` dies in its venv step
//! (`uv`'s CPython has no working `ensurepip`) and again on `openocd`
//! (`libusb-1.0.so.0`). Both are host packaging accidents, but the shape of the
//! failure is the real lesson: the environment has to be provisioned by fbuild
//! like every other toolchain, not by a script that assumes a system Python.
//!
//! A host `python3` cannot stand in for it either. This host's is 3.14, and half
//! of IDF's requirements publish no `cp314` wheel; the environment must be built
//! from a *pinned* interpreter so the wheel tags the lock resolves are the tags
//! the interpreter actually has.
//!
//! ## Two pins, both sha256
//!
//! 1. **The interpreter** — a python-build-standalone `install_only` archive per
//!    host ([`PYTHON_INTERPRETERS`]), sha256-pinned and byte-measured. It ships
//!    pip and setuptools, so `python -m venv` and the install below work offline
//!    apart from the wheel downloads, and no `ensurepip` is ever needed from the
//!    host.
//! 2. **The packages** — `data/esp-idf-v5.5.1-python.lock`: the pinned commit's
//!    `tools/requirements/requirements.core.txt` resolved against the release's
//!    published `espidf.constraints.v5.5.txt` (both committed beside the lock)
//!    with `uv pip compile --universal --generate-hashes`. Installed with
//!    `pip install --require-hashes --no-deps`, so every wheel *and* every sdist
//!    must match a listed sha256 or the install fails; `--universal` means one
//!    file is correct on all five hosts, because each line carries the hashes of
//!    every platform's file.
//!
//! The lock, not the checkout, is the pin: provisioning the environment never
//! reads the source tree. That is why it can be installed and measured (85.5 MB of venv,
//! `du -sb`) before the 450 MB tree is.
//!
//! ## What is proven, and where
//!
//! The end-to-end test below installs the real archive and runs ESP-IDF's own
//! `check_python_dependencies.py` against it, which is the same gate `idf.py`
//! runs on startup — not a proxy. Only `linux-amd64` has been exercised that
//! way; the other four interpreter pins come from python-build-standalone's
//! published `SHA256SUMS` (the same class of upstream metadata as
//! `tools.json` in the parent module) and are byte-verified metadata, not yet a
//! host proof.

use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use fbuild_core::path::NormalizedPath;
use fbuild_core::platform::executable::native_name;
use fbuild_core::platform::host::{self, HostPlatform};
use fbuild_core::subprocess::{ToolOutput, run_command};
use fbuild_core::{FbuildError, Result};

use super::esp_idf_release::{IDF_RELEASE_VERSION, platform_key};
use super::esp_idf_release_profile::dir_bytes;
use crate::{CacheSubdir, PackageBase, PackageInfo};

/// python-build-standalone release the interpreter pins come from.
pub const PYTHON_RELEASE_TAG: &str = "20261026";

/// CPython version every pin in this module is built from. The minor version is
/// what the wheel tags in the lock are resolved for (`cp312`).
pub const PYTHON_VERSION: &str = "3.12.16";

/// The directory name ESP-IDF itself derives from `python_env/`: `idf<major.minor>_py<minor>_env`
/// (`idf_tools.py::get_python_env_path`). Spelled the same here so the tree can
/// later be handed to `idf.py` without a translation layer.
pub const IDF_PYTHON_ENV_NAME: &str = "idf5.5_py3.12_env";

/// The hash-locked requirements of the pinned commit. `include_str!` so the
/// pins travel inside the binary and cannot drift from the code that uses them.
pub const REQUIREMENTS_LOCK: &str = include_str!("../../data/esp-idf-v5.5.1-python.lock");

/// The release's published constraints file, committed for reproducible
/// regeneration of the lock and for the checker below (`-c`).
pub const REQUIREMENTS_CONSTRAINTS: &str =
    include_str!("../../data/esp-idf-v5.5.1-constraints.txt");

/// Name the constraints file keeps once written into the cache: this is the
/// name `idf_tools.py` looks for under `IDF_TOOLS_PATH`.
pub const CONSTRAINTS_FILENAME: &str = "espidf.constraints.v5.5.txt";

const LOCK_FILENAME: &str = "idf-requirements.lock";
const ENV_OK_MARKER: &str = "requirements-installed.marker";
const INTERPRETER_DIR: &str = "python";
const ENV_PARENT_DIR: &str = "python_env";

/// Modules a delegated build imports before it touches a single source file.
/// Checked after the install so a truncated environment fails here, with the
/// import name, instead of inside a CMake configure 40 seconds later.
const REQUIRED_IMPORTS: &[&str] = &[
    "esp_idf_kconfig",
    "idf_component_manager",
    "esptool",
    "esp_idf_size",
];

const VENV_CREATE_TIMEOUT: Duration = Duration::from_secs(300);
const PIP_INSTALL_TIMEOUT: Duration = Duration::from_secs(900);
const IMPORT_CHECK_TIMEOUT: Duration = Duration::from_secs(120);

/// One host's pinned `install_only` CPython.
#[derive(Debug, Clone, Copy)]
pub struct PythonInterpreterPin {
    /// Platform key, shared with the parent module's `tools.json` spelling.
    pub platform: &'static str,
    /// python-build-standalone release asset.
    pub url: &'static str,
    /// sha256 of `url`, from the release's published `SHA256SUMS`.
    pub sha256: &'static str,
    /// Compressed bytes of `url`.
    pub bytes: u64,
}

/// Every host fbuild supports, one pinned interpreter each.
pub const PYTHON_INTERPRETERS: &[PythonInterpreterPin] = &[
    PythonInterpreterPin {
        platform: "linux-amd64",
        url: "https://github.com/astral-sh/python-build-standalone/releases/download/20261026/cpython-3.12.16%2B20261026-x86_64-unknown-linux-gnu-install_only.tar.gz",
        sha256: "6a79e922e50006b601f29e688407659da1c2b86ebcd4e9c7254cf234be8b89de",
        bytes: 67921566,
    },
    PythonInterpreterPin {
        platform: "linux-arm64",
        url: "https://github.com/astral-sh/python-build-standalone/releases/download/20261026/cpython-3.12.16%2B20261026-aarch64-unknown-linux-gnu-install_only.tar.gz",
        sha256: "a945427411dc6851a1393533b74a4a1ac434321ae397f2ec69897a9b4fde032b",
        bytes: 53051192,
    },
    PythonInterpreterPin {
        platform: "macos",
        url: "https://github.com/astral-sh/python-build-standalone/releases/download/20261026/cpython-3.12.16%2B20261026-x86_64-apple-darwin-install_only.tar.gz",
        sha256: "a962e47df522889eaa7ff0d6a56203f1079f68aabb65ffdbe8455f362bc769f3",
        bytes: 24873291,
    },
    PythonInterpreterPin {
        platform: "macos-arm64",
        url: "https://github.com/astral-sh/python-build-standalone/releases/download/20261026/cpython-3.12.16%2B20261026-aarch64-apple-darwin-install_only.tar.gz",
        sha256: "fbf4afef53c2a6b72bb741bc06d5671a9dfaf87bd84e5aa73ac9b3d0d589f14e",
        bytes: 25172086,
    },
    PythonInterpreterPin {
        platform: "win32",
        url: "https://github.com/astral-sh/python-build-standalone/releases/download/20261026/cpython-3.12.16%2B20261026-x86_64-pc-windows-msvc-install_only.tar.gz",
        sha256: "590dfcbe3a34c07301e6332e13e2188b98655cd072b08afa45a7998dc4c22bc5",
        bytes: 47113364,
    },
];

/// The pin for `host`, or an error naming the unsupported host.
pub fn interpreter_pin(host: HostPlatform) -> Result<&'static PythonInterpreterPin> {
    let key = platform_key(host).ok_or_else(|| {
        FbuildError::PackageError(format!(
            "no pinned ESP-IDF Python interpreter for {:?} {:?} (pinned: linux-amd64, linux-arm64, macos, macos-arm64, win32)",
            host.os(),
            host.arch()
        ))
    })?;
    PYTHON_INTERPRETERS
        .iter()
        .find(|pin| pin.platform == key)
        .ok_or_else(|| {
            FbuildError::PackageError(format!(
                "no pinned ESP-IDF Python interpreter for platform key {key}"
            ))
        })
}

/// A provisioned environment, as a delegated build needs to see it.
///
/// Deliberately just a directory plus the paths derived from it: the caller
/// (#1971) hands these to a subprocess and never has to know how the venv was
/// built.
#[derive(Debug, Clone)]
pub struct IdfPythonEnvironment {
    env_dir: NormalizedPath,
}

impl IdfPythonEnvironment {
    /// Wrap an existing `idf<ver>_py<ver>_env` directory.
    pub fn new(env_dir: NormalizedPath) -> Self {
        Self { env_dir }
    }

    /// The venv root (`.../python_env/idf5.5_py3.12_env`).
    pub fn env_dir(&self) -> &NormalizedPath {
        &self.env_dir
    }

    /// `.../bin/python` (`Scripts\python.exe` on Windows).
    pub fn python(&self) -> NormalizedPath {
        self.bin_dir().join(native_name("python"))
    }

    /// `bin` (`Scripts` on Windows): the one directory that has to be on `PATH`.
    pub fn bin_dir(&self) -> NormalizedPath {
        let leaf = if host::current().is_windows() {
            "Scripts"
        } else {
            "bin"
        };
        self.env_dir.join(leaf)
    }

    /// Variables `idf.py` and `idf_tools.py` read instead of deriving the env.
    ///
    /// `IDF_PYTHON_ENV_PATH` is the one that matters: both scripts take it over
    /// `IDF_TOOLS_PATH`, which is how a delegated build points at a cache entry
    /// that is not shaped like a full `~/.espressif`.
    pub fn env_vars(&self) -> Vec<(&'static str, NormalizedPath)> {
        vec![("IDF_PYTHON_ENV_PATH", self.env_dir.clone())]
    }
}

/// The pinned ESP-IDF Python environment, installed as one fbuild package.
pub struct EspIdfPython {
    base: PackageBase,
    root: NormalizedPath,
}

impl EspIdfPython {
    /// Resolve the pinned interpreter against the default cache.
    pub fn new(project_dir: &Path) -> Result<Self> {
        Self::build(project_dir, host::current(), None)
    }

    /// Install into an explicit cache root (tests and the end-to-end proof).
    #[cfg(test)]
    pub(super) fn with_cache_root(project_dir: &Path, cache_root: &Path) -> Result<Self> {
        Self::build(project_dir, host::current(), Some(cache_root))
    }

    fn build(project_dir: &Path, host: HostPlatform, cache_root: Option<&Path>) -> Result<Self> {
        let pin = interpreter_pin(host)?;
        // `<release>/python`: the IDF release is part of the key (the lock is
        // per release, not per interpreter), so two releases never share an env.
        let cache_key = format!("esp-idf-{IDF_RELEASE_VERSION}/python");
        let name = format!("esp-idf-{IDF_RELEASE_VERSION}-python");
        let version = format!("{PYTHON_VERSION}+{PYTHON_RELEASE_TAG}");
        let base = match cache_root {
            Some(root) => PackageBase::with_cache_root(
                &name,
                &version,
                pin.url,
                &cache_key,
                Some(pin.sha256),
                CacheSubdir::Toolchains,
                project_dir,
                root,
            ),
            None => PackageBase::new(
                &name,
                &version,
                pin.url,
                &cache_key,
                Some(pin.sha256),
                CacheSubdir::Toolchains,
                project_dir,
            ),
        };
        let root = NormalizedPath::new(base.install_path());
        Ok(Self { base, root })
    }

    /// The interpreter's install directory (its own cache entry).
    pub fn install_path(&self) -> &NormalizedPath {
        &self.root
    }

    /// The venv root IDF expects: `<entry>/python_env/idf5.5_py3.12_env`.
    pub fn env_dir(&self) -> NormalizedPath {
        self.root.join(ENV_PARENT_DIR).join(IDF_PYTHON_ENV_NAME)
    }

    /// Where the hash-locked requirements are written for `pip`.
    pub fn lock_path(&self) -> NormalizedPath {
        self.root.join(ENV_PARENT_DIR).join(LOCK_FILENAME)
    }

    /// Where the release's constraints file is written (the checker's `-c`).
    pub fn constraints_path(&self) -> NormalizedPath {
        self.root.join(ENV_PARENT_DIR).join(CONSTRAINTS_FILENAME)
    }

    /// True only when the interpreter is unpacked **and** the requirements were
    /// installed to completion: the last step writes the marker, so a venv that
    /// died during `pip install` is repaired rather than reused.
    pub fn is_installed(&self) -> bool {
        self.base.is_cached() && self.env_dir().join(ENV_OK_MARKER).as_path().is_file()
    }

    /// Install the interpreter, create the venv, and install the locked
    /// requirements. A second call on a complete environment is a no-op.
    pub async fn install(&self) -> Result<IdfPythonEnvironment> {
        let interpreter = self.ensure_interpreter().await?;
        let env = IdfPythonEnvironment::new(self.env_dir());
        if self.is_installed() {
            return Ok(env);
        }
        self.remove_partial_env()?;
        self.write_environment_files()?;
        create_venv(&interpreter, &env).await?;
        install_requirements(&env, &self.lock_path()).await?;
        check_imports(&env).await?;
        std::fs::write(env.env_dir().join(ENV_OK_MARKER).as_path(), b"ok\n")?;
        Ok(env)
    }

    /// Real bytes on disk of the whole cache entry, `du`-style (a hardlinked
    /// file counted once): the interpreter *and* the venv.
    pub fn installed_bytes(&self) -> u64 {
        let mut seen = HashSet::new();
        dir_bytes(self.root.as_path(), &mut seen)
    }

    /// Bytes of the venv alone.
    ///
    /// Kept separate from [`Self::installed_bytes`] because the two numbers
    /// answer different questions: #1970's size table quotes the Python
    /// *environment* (85 MB here, 63 MB for Espressif's), while the interpreter
    /// that carries the matching wheel tags is a fixed ~227 MB on top.
    pub fn venv_bytes(&self) -> u64 {
        let mut seen = HashSet::new();
        dir_bytes(self.env_dir().as_path(), &mut seen)
    }

    /// Package metadata for install reports and logging.
    pub fn get_info(&self) -> PackageInfo {
        self.base.get_info()
    }

    /// Unpack the pinned interpreter and return its directory.
    async fn ensure_interpreter(&self) -> Result<NormalizedPath> {
        if self.base.is_cached() {
            if let Ok(root) = interpreter_root(&self.root) {
                return Ok(root);
            }
        }
        let installed = self
            .base
            .staged_install(|root: &Path| interpreter_root_in(root).map(|_| ()))
            .await?;
        interpreter_root_in(&installed)
    }

    /// A half-built venv must not be resumed: `pip` would see a partial
    /// site-packages and a `.dist-info` set that disagrees with the lock.
    fn remove_partial_env(&self) -> Result<()> {
        let env = self.env_dir();
        if env.as_path().exists() {
            std::fs::remove_dir_all(env.as_path()).map_err(|err| {
                FbuildError::PackageError(format!("cannot clear partial venv {env}: {err}"))
            })?;
        }
        Ok(())
    }

    /// Write the two pinned files next to the venv, where a review can read
    /// exactly what was installed and the checker can find its constraints.
    fn write_environment_files(&self) -> Result<()> {
        let dir = self.root.join(ENV_PARENT_DIR);
        std::fs::create_dir_all(dir.as_path())?;
        std::fs::write(self.lock_path().as_path(), REQUIREMENTS_LOCK)?;
        std::fs::write(self.constraints_path().as_path(), REQUIREMENTS_CONSTRAINTS)?;
        Ok(())
    }
}

/// The interpreter directory inside an extracted `install_only` archive:
/// python-build-standalone nests everything under `python/`.
fn interpreter_root(root: &NormalizedPath) -> Result<NormalizedPath> {
    interpreter_root_in(root.as_path())
}

fn interpreter_root_in(root: &Path) -> Result<NormalizedPath> {
    let root = NormalizedPath::new(root);
    for candidate in [root.join(INTERPRETER_DIR), root.clone()] {
        if find_python(&candidate).is_some() {
            return Ok(candidate);
        }
    }
    Err(FbuildError::PackageError(format!(
        "no Python interpreter in {} (looked for {INTERPRETER_DIR}/bin/{})",
        root,
        native_name("python3")
    )))
}

/// The interpreter binary in `dir`, accepting both the POSIX layout (`bin/`) and
/// the Windows one (`python.exe` at the root).
fn find_python(dir: &NormalizedPath) -> Option<NormalizedPath> {
    let mut candidates = Vec::new();
    for stem in ["python3", "python"] {
        let exe = native_name(stem);
        candidates.push(dir.join(&exe));
        candidates.push(dir.join("bin").join(&exe));
    }
    candidates
        .into_iter()
        .find(|candidate| candidate.as_path().is_file())
}

fn interpreter_python(dir: &NormalizedPath) -> Result<NormalizedPath> {
    find_python(dir)
        .ok_or_else(|| FbuildError::PackageError(format!("no Python interpreter under {dir}")))
}

/// Create the venv with its own pip.
///
/// `--without-pip` is the obvious choice and it is wrong: nothing would be left
/// to install with. python-build-standalone bundles pip and setuptools, so the
/// plain form needs no network and no host `ensurepip`.
async fn create_venv(interpreter_dir: &NormalizedPath, env: &IdfPythonEnvironment) -> Result<()> {
    let interpreter = interpreter_python(interpreter_dir)?.to_string();
    let env_dir = env.env_dir().to_string();
    let output = run_command(
        &[&interpreter, "-m", "venv", &env_dir],
        None,
        None,
        Some(VENV_CREATE_TIMEOUT),
    )
    .await?;
    require_ok("python -m venv", &output)
}

/// Install the lock with hash checking on.
///
/// `--no-deps` is not an optimisation: the lock *is* the closure, so letting pip
/// resolve would let it install a dependency the lock does not pin.
async fn install_requirements(env: &IdfPythonEnvironment, lock: &NormalizedPath) -> Result<()> {
    let python = env.python().to_string();
    let lock = lock.to_string();
    let output = run_command(
        &[
            &python,
            "-m",
            "pip",
            "install",
            "--no-cache-dir",
            "--disable-pip-version-check",
            "--require-hashes",
            "--no-deps",
            "-r",
            &lock,
        ],
        None,
        Some(&[("PIP_NO_INPUT", "1")]),
        Some(PIP_INSTALL_TIMEOUT),
    )
    .await?;
    require_ok("pip install --require-hashes", &output)
}

/// Import the modules a build needs first: a failure here names the package.
async fn check_imports(env: &IdfPythonEnvironment) -> Result<()> {
    let python = env.python().to_string();
    let script = REQUIRED_IMPORTS
        .iter()
        .map(|module| format!("import {module}"))
        .collect::<Vec<_>>()
        .join("; ");
    let output = run_command(
        &[&python, "-c", &script],
        None,
        None,
        Some(IMPORT_CHECK_TIMEOUT),
    )
    .await?;
    require_ok("import check", &output)
}

fn require_ok(what: &str, output: &ToolOutput) -> Result<()> {
    if output.exit_code == 0 {
        return Ok(());
    }
    Err(FbuildError::PackageError(format!(
        "{what} failed with exit code {}: {}",
        output.exit_code,
        tail(&output.stderr, 2000)
    )))
}

/// Last `max` chars of `text`, on a char boundary: pip's real error is at the
/// end of a scroll of download lines.
fn tail(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut start = text.len() - max;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fbuild_core::platform::host::{HostArch, HostOs};

    /// Every host fbuild supports, in the parent module's platform-key spelling.
    const SUPPORTED_PLATFORMS: &[&str] = &[
        "linux-amd64",
        "linux-arm64",
        "macos",
        "macos-arm64",
        "win32",
    ];

    #[test]
    fn every_supported_host_has_exactly_one_pinned_interpreter() {
        assert_eq!(PYTHON_INTERPRETERS.len(), SUPPORTED_PLATFORMS.len());
        for key in SUPPORTED_PLATFORMS {
            let matches: Vec<_> = PYTHON_INTERPRETERS
                .iter()
                .filter(|pin| pin.platform == *key)
                .collect();
            assert_eq!(matches.len(), 1, "{key} must be pinned exactly once");
        }
    }

    #[test]
    fn interpreter_pins_are_distinct_hashed_assets_of_the_pinned_release() {
        let mut hashes: Vec<&str> = PYTHON_INTERPRETERS.iter().map(|p| p.sha256).collect();
        let total = hashes.len();
        hashes.sort_unstable();
        hashes.dedup();
        assert_eq!(total, hashes.len(), "two hosts share one sha256");
        for pin in PYTHON_INTERPRETERS {
            assert_eq!(pin.sha256.len(), 64, "{}: sha256 length", pin.platform);
            assert!(
                pin.sha256.chars().all(|c| c.is_ascii_hexdigit()),
                "{}: sha256 is not hex",
                pin.platform
            );
            // The URL must name both the interpreter version and the release, so
            // a pin cannot silently point at another build of "3.12".
            assert!(
                pin.url.contains(PYTHON_VERSION),
                "{}: {}",
                pin.platform,
                pin.url
            );
            assert!(
                pin.url.contains(PYTHON_RELEASE_TAG),
                "{}: {}",
                pin.platform,
                pin.url
            );
            assert!(
                pin.url.ends_with("-install_only.tar.gz"),
                "{}",
                pin.platform
            );
            assert!(
                pin.bytes > 20_000_000,
                "{}: a CPython archive is not {} B",
                pin.platform,
                pin.bytes
            );
        }
    }

    #[test]
    fn the_current_host_resolves_to_its_pin() {
        let pin = interpreter_pin(host::current()).expect("this host must be pinned");
        assert_eq!(
            Some(pin.platform),
            platform_key(host::current()),
            "pin lookup disagrees with the platform key"
        );
    }

    #[test]
    fn an_unsupported_host_names_what_is_pinned() {
        // 32-bit x86 Linux: no python-build-standalone install_only asset.
        let err = interpreter_pin(HostPlatform::new(HostOs::Linux, HostArch::X86))
            .expect_err("linux-i686 has no pin");
        let message = err.to_string();
        assert!(message.contains("linux-amd64"), "{message}");
        assert!(message.contains("win32"), "{message}");
    }

    /// Package lines: `name==version` at column 0, options indented below it.
    fn locked_packages() -> Vec<&'static str> {
        REQUIREMENTS_LOCK
            .lines()
            .filter(|line| !line.starts_with(char::is_whitespace) && !line.starts_with('#'))
            .filter_map(|line| line.split(" \\").next())
            .collect()
    }

    #[test]
    fn the_lock_pins_the_whole_core_requirement_set() {
        let packages = locked_packages();
        assert_eq!(packages.len(), 60, "lock package count moved: {packages:?}");
        // The 21 direct entries of requirements.core.txt, by normalized name.
        // A regenerated lock that loses one is not a smaller install, it is a
        // broken environment that fails inside `idf.py` instead of here.
        let names: Vec<&str> = packages
            .iter()
            .map(|p| p.split('=').next().unwrap_or_default())
            .collect();
        for required in [
            "setuptools",
            "packaging",
            "click",
            "pyserial",
            "cryptography",
            "pyparsing",
            "pyelftools",
            "idf-component-manager",
            "esp-coredump",
            "esptool",
            "esp-idf-kconfig",
            "esp-idf-monitor",
            "esp-idf-nvs-partition-gen",
            "esp-idf-size",
            "esp-idf-diag",
            "esp-idf-panic-decoder",
            "pyclang",
            "construct",
            "rich",
            "psutil",
            "freertos-gdb",
        ] {
            assert!(names.contains(&required), "{required} is not pinned");
        }
    }

    #[test]
    fn every_locked_package_is_versioned_and_hash_checked() {
        for package in locked_packages() {
            assert!(
                package.split("==").nth(1).is_some_and(|v| !v.is_empty()),
                "not an exact pin: {package}"
            );
        }
        // One hash per platform file; a `--require-hashes` install with a single
        // hash per package would only work on the host the lock was made on.
        let hashes = REQUIREMENTS_LOCK.matches("--hash=sha256:").count();
        assert!(hashes > 900, "only {hashes} hashed files in the lock");
        for banned in ["-e ", "--index-url", "--extra-index-url", "--trusted-host"] {
            assert!(
                !REQUIREMENTS_LOCK.contains(banned),
                "lock contains {banned}: the install would stop being hash-pinned"
            );
        }
    }

    /// The constraints file is what makes `esptool~=4.12` (sdist-only on PyPI)
    /// and `--only-binary cryptography` reproducible; it is committed beside the
    /// lock so the pair can be regenerated without the checkout.
    #[test]
    fn the_committed_constraints_match_the_locked_versions() {
        assert!(
            REQUIREMENTS_CONSTRAINTS.contains("esptool~=4.12"),
            "esptool pin moved"
        );
        assert!(
            REQUIREMENTS_CONSTRAINTS.contains("cryptography>=2.1.4,<45"),
            "cryptography ceiling moved"
        );
        let packages = locked_packages();
        assert!(
            packages.contains(&"esptool==4.13.0"),
            "esptool: {packages:?}"
        );
        assert!(
            packages.contains(&"cryptography==44.0.3"),
            "cryptography: {packages:?}"
        );
    }

    #[test]
    fn the_environment_layout_is_the_one_idf_derives() {
        let tmp = tempfile::TempDir::new().unwrap();
        let python = EspIdfPython::with_cache_root(tmp.path(), tmp.path()).unwrap();
        let env = python.env_dir();
        assert_eq!(
            env.as_path().file_name().and_then(|n| n.to_str()),
            Some(IDF_PYTHON_ENV_NAME)
        );
        assert_eq!(
            env.as_path()
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str()),
            Some(ENV_PARENT_DIR)
        );
        assert!(env.as_path().is_absolute(), "env dir must be rooted");
        assert!(!python.is_installed(), "nothing is installed yet");
    }

    #[test]
    fn a_venv_without_the_completion_marker_is_not_installed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let python = EspIdfPython::with_cache_root(tmp.path(), tmp.path()).unwrap();
        std::fs::create_dir_all(python.install_path().as_path()).unwrap();
        std::fs::write(
            python.install_path().join(".install_complete").as_path(),
            b"x",
        )
        .unwrap();
        // A venv that died in `pip install` looks exactly like this.
        std::fs::create_dir_all(python.env_dir().as_path()).unwrap();
        std::fs::write(python.env_dir().join("pyvenv.cfg").as_path(), b"home = x\n").unwrap();
        assert!(!python.is_installed());
        std::fs::write(python.env_dir().join(ENV_OK_MARKER).as_path(), b"ok\n").unwrap();
        assert!(python.is_installed());
    }

    #[test]
    fn the_interpreter_is_found_in_both_archive_layouts() {
        let tmp = tempfile::TempDir::new().unwrap();
        let nested = tmp.path().join(INTERPRETER_DIR).join("bin");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join(native_name("python3")), b"#!/bin/true\n").unwrap();
        let root = interpreter_root_in(tmp.path()).unwrap();
        assert_eq!(
            root.as_path().file_name().and_then(|n| n.to_str()),
            Some(INTERPRETER_DIR)
        );
        assert!(interpreter_python(&root).is_ok());

        // A flattened archive (the POSIX layout of a re-packed tree).
        let flat = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(flat.path().join("bin")).unwrap();
        std::fs::write(
            flat.path().join("bin").join(native_name("python")),
            b"#!/bin/true\n",
        )
        .unwrap();
        assert!(interpreter_root_in(flat.path()).is_ok());
    }

    #[test]
    fn an_empty_install_directory_is_not_an_interpreter() {
        let tmp = tempfile::TempDir::new().unwrap();
        let err = interpreter_root_in(tmp.path()).expect_err("no python there");
        assert!(err.to_string().contains("no Python interpreter"), "{err}");
    }

    #[test]
    fn installed_bytes_measures_the_venv_inside_the_entry() {
        let tmp = tempfile::TempDir::new().unwrap();
        let python = EspIdfPython::with_cache_root(tmp.path(), tmp.path()).unwrap();
        let marker_dir = python.env_dir().join("lib");
        std::fs::create_dir_all(marker_dir.as_path()).unwrap();
        std::fs::write(marker_dir.join("blob.bin").as_path(), vec![0u8; 4096]).unwrap();
        assert!(python.installed_bytes() >= 4096);
    }

    #[test]
    fn the_error_tail_is_bounded_and_never_splits_a_char() {
        let long = format!("{}é", "x".repeat(100));
        let tailed = tail(&long, 10);
        assert!(tailed.len() <= 10);
        assert!(tailed.ends_with('é'));
        assert_eq!(tail("short", 100), "short");
    }

    /// Real install: downloads the pinned CPython (68 MB) and the hashed wheels
    /// (~30 MB), creates the venv, and (when `FBUILD_IDF_SOURCE_DIR` points at a
    /// checkout of the pinned commit) runs ESP-IDF's own
    /// `tools/check_python_dependencies.py` — the same gate `idf.py` runs on
    /// startup. Run with:
    /// `soldr cargo test -p fbuild-toolchain --lib -- --ignored esp_idf_python_env`
    #[tokio::test]
    #[ignore = "downloads the pinned CPython interpreter and ~30 MB of hashed wheels"]
    async fn esp_idf_python_env_satisfies_the_idf_dependency_checker() {
        let dir = tempfile::TempDir::new().unwrap();
        let python = EspIdfPython::with_cache_root(dir.path(), dir.path()).unwrap();
        assert!(!python.is_installed());
        let env = python.install().await.unwrap();
        assert!(python.is_installed());
        assert!(env.python().as_path().is_file(), "no venv python");
        assert!(
            python.lock_path().as_path().is_file() && python.constraints_path().as_path().is_file(),
            "the locked inputs must be on disk next to the venv"
        );

        let py = env.python().to_string();
        let version = fbuild_core::subprocess::run_command(
            &[&py, "-c", "import sys; print(sys.version_info[:2])"],
            None,
            None,
            None,
        )
        .await
        .unwrap();
        assert!(version.success(), "{}", version.stderr);
        assert!(
            version.stdout.contains("3, 12"),
            "interpreter is {}",
            version.stdout
        );

        if let Ok(source) = std::env::var("FBUILD_IDF_SOURCE_DIR") {
            let checker = std::path::Path::new(&source).join("tools/check_python_dependencies.py");
            let requirements =
                std::path::Path::new(&source).join("tools/requirements/requirements.core.txt");
            let constraints = python.constraints_path().to_string();
            let output = fbuild_core::subprocess::run_command(
                &[
                    &py,
                    &checker.to_string_lossy(),
                    "-c",
                    &constraints,
                    "-r",
                    &requirements.to_string_lossy(),
                ],
                None,
                None,
                None,
            )
            .await
            .unwrap();
            assert!(
                output.success(),
                "ESP-IDF rejected the environment: {}\n{}",
                output.stdout,
                output.stderr
            );
            println!("{}", output.stdout.trim());
            assert!(
                output.stdout.contains("requirements are satisfied"),
                "unexpected checker output: {}",
                output.stdout
            );
        }

        let venv_bytes = python.venv_bytes();
        let total_bytes = python.installed_bytes();
        println!("venv {venv_bytes} B, entry {total_bytes} B");
        assert!(
            (50_000_000..120_000_000).contains(&venv_bytes),
            "venv measured {venv_bytes} B"
        );
        assert!(
            (250_000_000..420_000_000).contains(&total_bytes),
            "entry measured {total_bytes} B"
        );
        // AC4: a second install is a no-op, not a re-download.
        let again = python.install().await.unwrap();
        assert_eq!(env.env_dir(), again.env_dir());
    }
}
