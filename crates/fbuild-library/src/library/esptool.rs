//! `tool-esptoolpy` provisioning (FastLED/fbuild#954).
//!
//! ESP32 builds need `esptool` for the `elf2image` step that converts
//! `firmware.elf` → the flashable `firmware.bin` (and, when only a bootloader
//! ELF ships, `bootloader.bin`). Historically fbuild shelled out to an
//! `esptool` on `PATH`, which fails on a pristine machine with
//! "esptool not found — Install with: pip install esptool". This module
//! provisions esptool as a managed package instead, so no user `pip install`
//! is required.
//!
//! Modern metadata URLs on Unix use the **PyInstaller standalone binary** from
//! [`tasmota/esptool`](https://github.com/tasmota/esptool). Older pioarduino
//! releases instead pin a source-only `pioarduino/esptool` archive; those are
//! installed at the exact URL in a private `uv tool` environment so their
//! Python dependencies do not leak into fbuild's runtime.
//!
//! Flow:
//! 1. The version is taken from the pioarduino `tool-esptoolpy` metadata URL,
//!    which comes in two shapes: version-in-filename
//!    (`.../download/0.0.1/esptoolpy-v5.3.0.zip` → `5.3.0`) and
//!    version-in-release-tag (`.../download/v4.8.5/esptool.zip` → `4.8.5`).
//!    See `extract_esptool_version`.
//! 2. Windows registry metadata selects the published pioarduino source archive
//!    at the same version: some releases (including 5.1.0) have no Windows binary.
//!    Other hosts map `(OS, ARCH)` to a tasmota platform tag
//!    (`linux-amd64`, `macos-arm64`, `windows-amd64`, …). An unsupported host
//!    yields an error, and the caller falls back to an `esptool` on PATH.
//! 3. `https://github.com/tasmota/esptool/releases/download/v{version}/esptool-{platform}.zip`
//!    is downloaded + extracted via the shared [`PackageBase::staged_install`]
//!    pattern, and the `esptool` executable is located inside it.
//!
//! [`ESPTOOL_PATH_ENV_VAR`] (`FBUILD_ESPTOOL_PATH`) short-circuits the whole
//! flow with a caller-supplied executable, matching the override every other
//! provisioned tool already has (`FBUILD_WCHISP_PATH`, `FBUILD_PROBE_RS_PATH`,
//! …). It is the only escape hatch that survives the daemon's `env_clear`,
//! which strips everything but the `FBUILD_` prefix (FastLED/fbuild#1220).

use std::path::Path;

use fbuild_core::{FbuildError, Result, path::NormalizedPath, subprocess::run_command};

use crate::{CacheSubdir, PackageBase};

/// Environment variable that overrides esptool resolution with an explicit
/// executable path, bypassing provisioning entirely (FastLED/fbuild#1220).
pub const ESPTOOL_PATH_ENV_VAR: &str = "FBUILD_ESPTOOL_PATH";

/// Resolve [`ESPTOOL_PATH_ENV_VAR`] into an executable path.
///
/// * unset (or set to an empty/whitespace-only value) → `Ok(None)`, provision
///   normally.
/// * set and naming a file → `Ok(Some(path))`.
/// * set and NOT naming a file → `Err`. An explicit override that silently
///   degraded to provisioning would defeat the purpose of the escape hatch:
///   the user set it precisely because provisioning is what's broken.
pub fn esptool_path_override() -> Result<Option<NormalizedPath>> {
    let raw = match std::env::var_os(ESPTOOL_PATH_ENV_VAR) {
        Some(v) => v,
        None => return Ok(None),
    };
    let path = Path::new(&raw);
    if path.as_os_str().is_empty() || raw.to_string_lossy().trim().is_empty() {
        return Ok(None);
    }
    if path.is_file() {
        return Ok(Some(NormalizedPath::from(path)));
    }
    Err(FbuildError::PackageError(format!(
        "{ESPTOOL_PATH_ENV_VAR} does not name a file: {}",
        path.display()
    )))
}

/// Managed `tool-esptoolpy` package (standalone binary or pinned source).
///
/// Constructed from the `platform.json` metadata URL (used only to extract the
/// pinned version) and resolved lazily in [`Self::ensure_installed`], which
/// returns the path to the `esptool` executable.
pub struct Esptool {
    project_dir: NormalizedPath,
    version: String,
    source_url: Option<String>,
    source_checksum: Option<String>,
    expected_runtime_version: Option<String>,
}

impl Esptool {
    /// Create from the `platform.json`-derived `tool-esptoolpy` URL
    /// (`Esp32Platform::get_package_url("tool-esptoolpy")`). Only the version
    /// embedded in registry URLs is used — from the filename. Direct source
    /// archive URLs are retained, with the version taken from the release tag.
    pub fn from_metadata_url(project_dir: &Path, metadata_url: &str) -> Self {
        Self::from_metadata_url_for_host(
            project_dir,
            metadata_url,
            fbuild_core::platform::host::is_windows(),
        )
    }

    fn from_metadata_url_for_host(project_dir: &Path, metadata_url: &str, windows: bool) -> Self {
        let version = extract_esptool_version(metadata_url);
        let source_url = if metadata_url
            .starts_with("https://github.com/pioarduino/esptool/releases/download/")
        {
            Some(metadata_url.to_string())
        } else if windows
            && metadata_url.starts_with("https://github.com/pioarduino/registry/releases/download/")
            && version != "unknown"
            && metadata_url.ends_with(&format!("/esptoolpy-v{version}.zip"))
        {
            // This is the any-host source URL published in the registry's
            // tools.json, not a renamed binary asset. Keep the SDK's version;
            // ensure_source_installed verifies the resulting uv launcher.
            Some(format!(
                "https://github.com/pioarduino/esptool/releases/download/v{version}/esptool.zip"
            ))
        } else {
            None
        };
        Self {
            project_dir: NormalizedPath::from(project_dir),
            version: version.clone(),
            source_url,
            source_checksum: None,
            expected_runtime_version: Some(version),
        }
    }

    /// Use the exact PlatformIO-hosted source archive and published digest.
    /// PlatformIO's package version (e.g. `2.41100.0`) is not esptool's CLI
    /// version (`4.11.0`), so the digest, not a converted version string,
    /// establishes package identity.
    pub fn from_registry_payload(
        project_dir: &Path,
        payload: &fbuild_core::platformio_package::ResolvedPayload,
    ) -> Self {
        Self {
            project_dir: NormalizedPath::from(project_dir),
            version: payload.version.clone(),
            source_url: Some(payload.url.clone()),
            source_checksum: Some(payload.sha256.clone()),
            expected_runtime_version: None,
        }
    }

    /// The esptool version parsed out of the metadata URL, or `"unknown"` when
    /// neither URL shape carried a dotted version. Reported in provisioning
    /// diagnostics so a bad parse is visible at the point of failure rather
    /// than three minutes later at `elf2image` (FastLED/fbuild#1220).
    pub fn version(&self) -> &str {
        &self.version
    }

    /// The tasmota release URL this package would download, or an error when
    /// the host has no prebuilt binary. Reported alongside [`Self::version`]
    /// when provisioning fails.
    pub fn download_url(&self) -> Result<String> {
        if let Some(url) = &self.source_url {
            return Ok(url.clone());
        }
        Ok(Self::release_url(&self.version, host_platform_tag()?))
    }

    fn release_url(version: &str, platform: &str) -> String {
        format!(
            "https://github.com/tasmota/esptool/releases/download/v{version}/esptool-{platform}.zip"
        )
    }

    /// The esptool binary [`Self::ensure_installed`] would return, if it is
    /// already usable: the [`ESPTOOL_PATH_ENV_VAR`] override, or a cached
    /// install that contains the executable. Never installs anything, so
    /// `fbuild install --check` can ask it (FastLED/fbuild#1433).
    pub fn installed_binary(&self) -> Result<Option<NormalizedPath>> {
        if let Some(override_path) = esptool_path_override()? {
            return Ok(Some(override_path));
        }
        if let Some(url) = &self.source_url {
            let base = self.source_package(url);
            return Ok(base
                .is_cached()
                .then(|| source_esptool_binary(&base.install_path()))
                .flatten());
        }
        let url = Self::release_url(&self.version, host_platform_tag()?);
        let base = PackageBase::new(
            "tool-esptoolpy",
            &self.version,
            &url,
            &url,
            None,
            CacheSubdir::Toolchains,
            self.project_dir.as_path(),
        );
        // An interrupted install can hold the executable without the
        // completion sentinel; that is not an install.
        if !base.is_cached() {
            return Ok(None);
        }
        Ok(find_esptool_binary(&base.install_path()))
    }

    /// Ensure the standalone esptool binary is installed and return its path.
    /// The caller runs it directly as `<bin> --chip <chip> elf2image …`.
    ///
    /// [`ESPTOOL_PATH_ENV_VAR`] short-circuits provisioning when set; a value
    /// that doesn't name a file is a hard error, not a silent fallback.
    ///
    /// Cache-aware: installs via the shared [`PackageBase::staged_install`]
    /// pattern, so a warm cache costs no network I/O. Returns an error on an
    /// unsupported host or a missing binary, so the caller can fall back to an
    /// `esptool` on PATH.
    pub async fn ensure_installed(&self) -> Result<NormalizedPath> {
        if let Some(override_path) = esptool_path_override()? {
            tracing::info!(
                "using esptool from {}: {}",
                ESPTOOL_PATH_ENV_VAR,
                override_path.display()
            );
            return Ok(override_path);
        }

        if let Some(url) = &self.source_url {
            return self.ensure_source_installed(url).await;
        }

        let platform = host_platform_tag()?;
        let url = Self::release_url(&self.version, platform);

        let base = PackageBase::new(
            "tool-esptoolpy",
            &self.version,
            &url,
            &url,
            None,
            CacheSubdir::Toolchains,
            self.project_dir.as_path(),
        );
        remove_invalid_cached_install(&base.install_path())?;
        let install_path = base.staged_install(validate_esptool).await?;

        let bin = find_esptool_binary(&install_path).ok_or_else(|| {
            FbuildError::PackageError(format!(
                "esptool executable not found under {}",
                install_path.display()
            ))
        })?;

        // The GitHub-released zips do not always preserve the executable bit;
        // set it every time (idempotent, cheap) so a cached install stays
        // runnable.
        let _ = fbuild_core::platform::fs::ensure_executable(bin.as_path());

        if let Err(error) = verify_esptool_once(bin.as_path(), &install_path, None).await {
            if let Err(remove_error) = remove_cached_install(&install_path) {
                tracing::warn!(
                    path = %install_path.display(),
                    error = %remove_error,
                    "failed to remove unusable cached esptool install"
                );
            }
            return Err(error);
        }

        Ok(bin)
    }

    fn source_package(&self, url: &str) -> PackageBase {
        PackageBase::new(
            "tool-esptoolpy",
            &self.version,
            url,
            url,
            self.source_checksum.as_deref(),
            CacheSubdir::Toolchains,
            self.project_dir.as_path(),
        )
    }

    /// Older pioarduino manifests publish esptool as Python source, not a
    /// standalone executable. Install the exact archive in its own managed
    /// environment so its declared dependencies cannot contaminate fbuild.
    async fn ensure_source_installed(&self, url: &str) -> Result<NormalizedPath> {
        let base = self.source_package(url);
        let install_path = base.staged_install(validate_source_esptool).await?;
        validate_source_esptool(&install_path)?;
        let binary = source_esptool_path(&install_path);
        if !binary.is_file() {
            let tool_dir = source_tools_dir(&install_path);
            let bin_dir = source_bin_dir(&install_path);
            let tool_dir_arg = tool_dir.to_string_lossy();
            let bin_dir_arg = bin_dir.to_string_lossy();
            let source_arg = install_path.to_string_lossy();
            let env = [
                ("UV_TOOL_DIR", tool_dir_arg.as_ref()),
                ("UV_TOOL_BIN_DIR", bin_dir_arg.as_ref()),
            ];
            run_uv(
                &["uv", "tool", "install", "--python", "3.10", &source_arg],
                Some(&env),
            )
            .await?;
        }
        verify_esptool_once(
            &binary,
            &install_path,
            self.expected_runtime_version.as_deref(),
        )
        .await
        .map_err(|error| {
            FbuildError::PackageError(format!("pinned esptool archive {url}: {error}"))
        })?;
        Ok(binary)
    }
}

fn validate_source_esptool(dir: &Path) -> Result<()> {
    if (dir.join("pyproject.toml").is_file() || dir.join("setup.py").is_file())
        && dir.join("esptool.py").is_file()
    {
        Ok(())
    } else {
        Err(FbuildError::PackageError(format!(
            "pinned esptool source archive is incomplete: {}",
            dir.display()
        )))
    }
}

fn source_tools_dir(dir: &Path) -> NormalizedPath {
    NormalizedPath::new(dir.join(format!("{}-tools", fbuild_paths::FBUILD_DIR_NAME)))
}

fn source_bin_dir(dir: &Path) -> NormalizedPath {
    NormalizedPath::new(dir.join(format!("{}-bin", fbuild_paths::FBUILD_DIR_NAME)))
}

fn source_esptool_path(dir: &Path) -> NormalizedPath {
    source_esptool_path_for_host(dir, fbuild_core::platform::host::is_windows())
}

fn source_esptool_path_for_host(dir: &Path, windows: bool) -> NormalizedPath {
    let tools_dir = source_tools_dir(dir);
    if windows {
        // uv removes the .py suffix when generating Windows console launchers.
        tools_dir.join("esptool/Scripts/esptool.exe")
    } else {
        tools_dir.join("esptool/bin/esptool.py")
    }
}

fn source_esptool_binary(dir: &Path) -> Option<NormalizedPath> {
    let path = source_esptool_path(dir);
    path.is_file().then_some(path)
}

async fn run_uv(args: &[&str], env: Option<&[(&str, &str)]>) -> Result<()> {
    let output = run_command(args, None, env, Some(std::time::Duration::from_secs(300)))
        .await
        .map_err(|error| {
            FbuildError::PackageError(format!(
                "pinned esptool source needs uv for isolated Python dependencies: {error}"
            ))
        })?;
    if output.success() {
        Ok(())
    } else {
        Err(FbuildError::PackageError(format!(
            "uv could not install pinned esptool source (exit {}): {} {}",
            output.exit_code, output.stdout, output.stderr
        )))
    }
}

/// Validation callback for [`PackageBase::staged_install`]: the extracted tree
/// must contain an `esptool` executable.
fn validate_esptool(dir: &Path) -> Result<()> {
    if find_esptool_binary(dir).is_some() {
        Ok(())
    } else {
        Err(FbuildError::PackageError(format!(
            "extracted esptool package has no esptool executable (in {})",
            dir.display()
        )))
    }
}

/// Remove a stale cache entry so [`PackageBase::staged_install`] can replace it.
///
/// `staged_install` trusts an existing install directory, while an Actions cache
/// restore can leave that directory without the standalone executable. Validate
/// this package-specific cache hit before taking that fast path.
fn remove_invalid_cached_install(install_path: &Path) -> Result<()> {
    if !install_path.exists() || validate_esptool(install_path).is_ok() {
        return Ok(());
    }

    tracing::warn!(
        path = %install_path.display(),
        "removing cached esptool install without an executable"
    );
    remove_cached_install(install_path)
}

fn remove_cached_install(install_path: &Path) -> Result<()> {
    match std::fs::remove_dir_all(install_path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(FbuildError::PackageError(format!(
            "failed to remove cached esptool install {}: {}",
            install_path.display(),
            e
        ))),
    }
}

/// Verify that the standalone executable can actually be launched.
///
/// `Path::is_file` is insufficient: a restored cache can retain a regular
/// file whose interpreter or dynamic loader is unavailable, which surfaces as
/// `ENOENT` only when the later `elf2image` command is spawned.
/// Left in an install whose esptool already passed [`verify_esptool_once`].
const VERIFIED_STAMP: &str = "esptool-verified.stamp";

/// Run `<bin> version` once per install instead of on every build: each
/// spawn starts a Python interpreter, which cost ~140 ms on every ESP32 build,
/// warm ones included (FastLED/fbuild#1540). The stamp records the binary's
/// size and mtime plus the expected version, so a replaced binary or a new pin
/// verifies again. Failures are never stamped.
async fn verify_esptool_once(
    bin: &Path,
    install: &Path,
    expected_version: Option<&str>,
) -> Result<()> {
    let stamp_path = install.join(VERIFIED_STAMP);
    let stamp = verified_stamp(bin, expected_version);
    if stamp.is_some() && std::fs::read_to_string(&stamp_path).ok() == stamp {
        return Ok(());
    }
    let output = verify_esptool_binary(bin).await?;
    if let Some(version) = expected_version {
        let expected = format!("v{version}");
        if !output.stdout.contains(&expected) && !output.stderr.contains(&expected) {
            return Err(FbuildError::PackageError(format!(
                "esptool at {} is not the pinned version; expected {version}",
                bin.display()
            )));
        }
    }
    if let Some(stamp) = stamp {
        if let Err(error) = std::fs::write(&stamp_path, stamp) {
            tracing::debug!(path = %stamp_path.display(), %error, "could not stamp verified esptool");
        }
    }
    Ok(())
}

fn verified_stamp(bin: &Path, expected_version: Option<&str>) -> Option<String> {
    let meta = std::fs::metadata(bin).ok()?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some(format!(
        "{}\n{}\n{mtime}\n{}\n",
        bin.display(),
        meta.len(),
        expected_version.unwrap_or("")
    ))
}

async fn verify_esptool_binary(bin: &Path) -> Result<fbuild_core::subprocess::ToolOutput> {
    let bin_arg = bin.to_string_lossy();
    let output = run_command(
        &[bin_arg.as_ref(), "version"],
        None,
        None,
        Some(std::time::Duration::from_secs(10)),
    )
    .await
    .map_err(|e| {
        FbuildError::PackageError(format!(
            "cached esptool executable {} cannot run: {}",
            bin.display(),
            e
        ))
    })?;
    if output.success() {
        Ok(output)
    } else {
        // Include the captured output. A bare "exited with status 2" is
        // undiagnosable, and that is exactly what a real user hit on Windows
        // with a freshly-installed 5.3.0 (FastLED/fbuild#1213 part 3).
        let mut detail = String::new();
        for (label, stream) in [("stderr", &output.stderr), ("stdout", &output.stdout)] {
            let text = stream.trim();
            if !text.is_empty() {
                detail.push_str(&format!("\n  {label}: {text}"));
            }
        }
        if detail.is_empty() {
            detail.push_str("\n  (no output on stdout or stderr)");
        }
        Err(FbuildError::PackageError(format!(
            "cached esptool executable {} exited with status {}{detail}",
            bin.display(),
            output.exit_code
        )))
    }
}

/// Map the host `(OS, ARCH)` to a tasmota esptool release platform tag.
///
/// Returns `None` for hosts without a prebuilt binary, so the caller falls
/// back to an `esptool` on PATH.
fn tasmota_platform_tag() -> Option<&'static str> {
    match (
        fbuild_core::platform::host::os_name(),
        fbuild_core::platform::host::arch_name(),
    ) {
        ("linux", "x86_64") => Some("linux-amd64"),
        ("linux", "aarch64") => Some("linux-aarch64"),
        ("linux", "arm") => Some("linux-armv7"),
        ("macos", "x86_64") => Some("macos-amd64"),
        ("macos", "aarch64") => Some("macos-arm64"),
        ("windows", "x86_64") => Some("windows-amd64"),
        _ => None,
    }
}

/// [`tasmota_platform_tag`] with the unsupported-host case turned into the
/// error both `ensure_installed` and `download_url` report.
fn host_platform_tag() -> Result<&'static str> {
    tasmota_platform_tag().ok_or_else(|| {
        FbuildError::PackageError(format!(
            "no prebuilt esptool binary for {}/{} — set {ESPTOOL_PATH_ENV_VAR} \
             to an esptool executable",
            fbuild_core::platform::host::os_name(),
            fbuild_core::platform::host::arch_name()
        ))
    })
}

/// Native executable name for the current platform (test fixtures).
#[cfg(test)]
fn esptool_bin_name() -> &'static str {
    fbuild_core::platform::executable::name("esptool", "esptool.exe")
}

/// Locate the `esptool` executable in an extracted tree, searching the root and
/// up to two levels deep (the tasmota zip nests it under
/// `esptool-<platform>/esptool`).
fn find_esptool_binary(root: &Path) -> Option<NormalizedPath> {
    fn search(dir: &Path, depth: usize) -> Option<NormalizedPath> {
        if let Some(candidate) = fbuild_core::platform::executable::find_tool_in(dir, "esptool") {
            return Some(candidate);
        }
        if depth == 0 {
            return None;
        }
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if let Some(found) = search(&path, depth - 1) {
                        return Some(found);
                    }
                }
            }
        }
        None
    }
    search(root, 2)
}

/// Scan a single path component for the first dotted numeric run, e.g.
/// `esptoolpy-v5.3.0` -> `5.3.0`, `v4.8.5` -> `4.8.5`. Returns `None` when the
/// component carries no dotted version (a bare `esptool` or a single `0`).
fn dotted_version_in(component: &str) -> Option<String> {
    let stem = component.trim_end_matches(".zip");
    let bytes = stem.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            let cand = stem[start..i].trim_end_matches('.');
            if cand.contains('.') {
                return Some(cand.to_string());
            }
        } else {
            i += 1;
        }
    }
    None
}

/// Extract a version string (e.g. `"5.3.0"`) from the pioarduino
/// `tool-esptoolpy` metadata URL.
///
/// Two URL shapes exist in the wild and they put the version in opposite
/// places, so we try them in a fixed order:
///
/// 1. **Filename first** — `.../releases/download/0.0.1/esptoolpy-v5.3.0.zip`,
///    where `0.0.1` is the *registry release tag* and the real esptool version
///    is in the filename. Parsing the filename first is what keeps the tag from
///    winning here.
/// 2. **Parent path segment as fallback** — `platform-espressif32 53.03.10`
///    publishes `.../releases/download/v4.8.5/esptool.zip`, where the filename
///    is generic and the version is the release tag. Before this fallback
///    existed the parse returned `"unknown"` for every build on that platform,
///    which 404s the download and silently degrades to a bare `esptool` PATH
///    lookup (FastLED/fbuild#1217).
///
/// Still falls back to `"unknown"` when neither component carries a dotted
/// version, rather than silently using the wrong one.
fn extract_esptool_version(url: &str) -> String {
    let mut segments = url.rsplit('/');
    let filename = segments.next().unwrap_or(url);
    if let Some(version) = dotted_version_in(filename) {
        return version;
    }
    // Only consulted when the filename carries no version of its own, so the
    // registry-tag case above is unaffected.
    if let Some(version) = segments.next().and_then(dotted_version_in) {
        return version;
    }
    "unknown".to_string()
}

#[cfg(test)]
#[path = "esptool_tests.rs"]
mod tests;
