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
//! We provision the **PyInstaller standalone binary** from the
//! [`tasmota/esptool`](https://github.com/tasmota/esptool) releases: a single
//! self-contained executable with every Python dependency (`rich_click`,
//! `pyserial`, …) bundled inside. This deliberately avoids the pioarduino
//! `esptoolpy-vX.Y.Z.zip`, which is pure-Python *source* WITHOUT its deps and
//! therefore dies at runtime with `ModuleNotFoundError: rich_click`. The
//! standalone binary needs no Python interpreter and no network at build time.
//!
//! Flow:
//! 1. The version is taken from the pioarduino `tool-esptoolpy` metadata URL,
//!    which comes in two shapes: version-in-filename
//!    (`.../download/0.0.1/esptoolpy-v5.3.0.zip` → `5.3.0`) and
//!    version-in-release-tag (`.../download/v4.8.5/esptool.zip` → `4.8.5`).
//!    See `extract_esptool_version`.
//! 2. The host `(OS, ARCH)` maps to a tasmota platform tag
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

/// Managed `tool-esptoolpy` package (tasmota standalone binary).
///
/// Constructed from the `platform.json` metadata URL (used only to extract the
/// pinned version) and resolved lazily in [`Self::ensure_installed`], which
/// returns the path to the `esptool` executable.
pub struct Esptool {
    project_dir: NormalizedPath,
    version: String,
}

impl Esptool {
    /// Create from the `platform.json`-derived `tool-esptoolpy` URL
    /// (`Esp32Platform::get_package_url("tool-esptoolpy")`). Only the version
    /// embedded in the URL is used — from the filename, or from the release
    /// tag when the filename is generic.
    pub fn from_metadata_url(project_dir: &Path, metadata_url: &str) -> Self {
        Self {
            project_dir: NormalizedPath::from(project_dir),
            version: extract_esptool_version(metadata_url),
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
        Ok(Self::release_url(&self.version, host_platform_tag()?))
    }

    fn release_url(version: &str, platform: &str) -> String {
        format!(
            "https://github.com/tasmota/esptool/releases/download/v{version}/esptool-{platform}.zip"
        )
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
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(bin.as_path()) {
                let mut perms = meta.permissions();
                if perms.mode() & 0o111 == 0 {
                    perms.set_mode(0o755);
                    let _ = std::fs::set_permissions(bin.as_path(), perms);
                }
            }
        }

        if let Err(error) = verify_esptool_binary(bin.as_path()).await {
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
async fn verify_esptool_binary(bin: &Path) -> Result<()> {
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
        Ok(())
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
    match (std::env::consts::OS, std::env::consts::ARCH) {
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
            std::env::consts::OS,
            std::env::consts::ARCH
        ))
    })
}

/// Executable name for the current platform.
fn esptool_bin_name() -> &'static str {
    if cfg!(windows) {
        "esptool.exe"
    } else {
        "esptool"
    }
}

/// Locate the `esptool` executable in an extracted tree, searching the root and
/// up to two levels deep (the tasmota zip nests it under
/// `esptool-<platform>/esptool`).
fn find_esptool_binary(root: &Path) -> Option<NormalizedPath> {
    fn search(dir: &Path, depth: usize) -> Option<NormalizedPath> {
        let candidate = dir.join(esptool_bin_name());
        if candidate.is_file() {
            return Some(NormalizedPath::from(candidate));
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
mod tests {
    use super::*;

    #[test]
    fn extract_version_from_pioarduino_metadata_url() {
        // The registry release tag (0.0.1) must NOT win over the real esptool
        // version embedded in the filename.
        assert_eq!(
            extract_esptool_version(
                "https://github.com/pioarduino/registry/releases/download/0.0.1/esptoolpy-v5.3.0.zip"
            ),
            "5.3.0"
        );
    }

    #[test]
    fn extract_version_from_bare_filename() {
        assert_eq!(extract_esptool_version("esptoolpy-v4.8.1.zip"), "4.8.1");
    }

    #[test]
    fn extract_version_from_release_tag_when_filename_is_generic() {
        // platform-espressif32 53.03.10 publishes the opposite shape: generic
        // filename, version in the release tag. Returning "unknown" here 404s
        // the download and degrades to a bare `esptool` PATH lookup, which
        // broke every ESP32 build on FastLED master (FastLED/fbuild#1217).
        assert_eq!(
            extract_esptool_version(
                "https://github.com/pioarduino/esptool/releases/download/v4.8.5/esptool.zip"
            ),
            "4.8.5"
        );
    }

    #[test]
    fn filename_version_still_wins_over_release_tag() {
        // Guards the ordering: both components carry a dotted version here, and
        // the filename's must win or we regress the registry-tag case.
        assert_eq!(
            extract_esptool_version(
                "https://github.com/pioarduino/registry/releases/download/0.0.1/esptoolpy-v5.3.0.zip"
            ),
            "5.3.0"
        );
    }

    #[test]
    fn extract_version_falls_back_to_unknown() {
        assert_eq!(
            extract_esptool_version("https://example.com/esptool.zip"),
            "unknown"
        );
    }

    #[test]
    fn platform_tag_is_known_for_this_host_or_none() {
        // Just assert the mapping is total over the match arms without panic;
        // the value depends on the build host.
        let tag = tasmota_platform_tag();
        if let Some(t) = tag {
            assert!(
                t.starts_with("linux-") || t.starts_with("macos-") || t.starts_with("windows-")
            );
        }
    }

    #[test]
    fn find_binary_at_root() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::write(root.join(esptool_bin_name()), b"bin").unwrap();
        let found = find_esptool_binary(root).unwrap();
        assert_eq!(found.as_path(), root.join(esptool_bin_name()));
    }

    #[test]
    fn find_binary_nested_one_level() {
        let tmp = tempfile::TempDir::new().unwrap();
        let inner = tmp.path().join("esptool-linux-amd64");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(inner.join(esptool_bin_name()), b"bin").unwrap();
        let found = find_esptool_binary(tmp.path()).unwrap();
        assert_eq!(found.as_path(), inner.join(esptool_bin_name()));
    }

    #[test]
    fn find_binary_missing_returns_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert_eq!(find_esptool_binary(tmp.path()), None);
    }

    #[test]
    fn validate_rejects_tree_without_binary() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(validate_esptool(tmp.path()).is_err());
    }

    #[test]
    fn invalid_cached_install_is_removed_for_reprovisioning() {
        let tmp = tempfile::TempDir::new().unwrap();
        let install = tmp.path().join("cached-esptool");
        std::fs::create_dir_all(&install).unwrap();
        std::fs::write(install.join("stale-marker"), b"incomplete").unwrap();

        remove_invalid_cached_install(&install).unwrap();

        assert!(
            !install.exists(),
            "an invalid cache hit must be removed before staged_install runs"
        );
    }

    #[test]
    fn valid_cached_install_is_preserved() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join(esptool_bin_name()), b"bin").unwrap();

        remove_invalid_cached_install(tmp.path()).unwrap();

        assert!(tmp.path().join(esptool_bin_name()).exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn verify_uses_supported_version_subcommand() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::TempDir::new().unwrap();
        let bin = tmp.path().join(esptool_bin_name());
        std::fs::write(
            &bin,
            b"#!/bin/sh\n[ \"$#\" -eq 1 ] && [ \"$1\" = \"version\" ]\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&bin).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&bin, permissions).unwrap();

        verify_esptool_binary(&bin).await.unwrap();
    }

    /// `FBUILD_ESPTOOL_PATH` is process-global; serialize the tests that touch
    /// it so a parallel run can't observe another test's value.
    static ESPTOOL_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Run `f` with `FBUILD_ESPTOOL_PATH` set to `value` (or unset for `None`),
    /// restoring whatever the environment had before.
    fn with_env_override<T>(value: Option<&Path>, f: impl FnOnce() -> T) -> T {
        let _guard = ESPTOOL_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let saved = std::env::var_os(ESPTOOL_PATH_ENV_VAR);
        match value {
            Some(v) => std::env::set_var(ESPTOOL_PATH_ENV_VAR, v),
            None => std::env::remove_var(ESPTOOL_PATH_ENV_VAR),
        }
        let out = f();
        match saved {
            Some(v) => std::env::set_var(ESPTOOL_PATH_ENV_VAR, v),
            None => std::env::remove_var(ESPTOOL_PATH_ENV_VAR),
        }
        out
    }

    #[test]
    fn override_unset_provisions_normally() {
        assert!(
            with_env_override(None, esptool_path_override)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn override_pointing_at_a_file_wins() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bin = tmp.path().join(esptool_bin_name());
        std::fs::write(&bin, b"stub").unwrap();

        let got = with_env_override(Some(&bin), esptool_path_override).unwrap();

        assert_eq!(got.map(|p| p.as_path().to_path_buf()), Some(bin));
    }

    /// The whole point of the escape hatch is that it's used when provisioning
    /// is broken; silently falling back to provisioning would hide the typo.
    #[test]
    fn override_pointing_nowhere_is_an_error_not_a_fallback() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ghost = tmp.path().join("does-not-exist");

        let err = with_env_override(Some(&ghost), esptool_path_override).unwrap_err();

        let msg = err.to_string();
        assert!(msg.contains(ESPTOOL_PATH_ENV_VAR), "{msg}");
        assert!(msg.contains("does-not-exist"), "{msg}");
    }

    /// A directory is not an executable — treat it like any other non-file.
    #[test]
    fn override_pointing_at_a_directory_is_an_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(with_env_override(Some(tmp.path()), esptool_path_override).is_err());
    }

    #[test]
    fn empty_override_is_treated_as_unset() {
        assert!(
            with_env_override(Some(Path::new("")), esptool_path_override)
                .unwrap()
                .is_none()
        );
    }

    /// The #1217 fault: an unparseable metadata URL yields version `unknown`,
    /// which builds a `vunknown` download URL that 404s. Reporting that URL is
    /// what makes the failure diagnosable at the point it happens.
    /// A value that is present but blank is a shell artifact
    /// (`FBUILD_ESPTOOL_PATH="$SOMETHING_UNSET"`), not a deliberate override,
    /// so it must read as unset rather than as a path that doesn't exist.
    #[test]
    fn whitespace_only_override_is_treated_as_unset() {
        assert!(
            with_env_override(Some(Path::new(" \t")), esptool_path_override)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn download_url_reports_the_parsed_version() {
        let tmp = tempfile::TempDir::new().unwrap();
        let esptool = Esptool::from_metadata_url(tmp.path(), "https://example.com/esptool.zip");

        assert_eq!(esptool.version(), "unknown");
        if let Ok(url) = esptool.download_url() {
            assert!(url.contains("/download/vunknown/"), "{url}");
        }
    }

    #[test]
    fn download_url_matches_the_url_ensure_installed_uses() {
        let tmp = tempfile::TempDir::new().unwrap();
        let esptool = Esptool::from_metadata_url(
            tmp.path(),
            "https://github.com/pioarduino/registry/releases/download/0.0.1/esptoolpy-v5.3.0.zip",
        );

        assert_eq!(esptool.version(), "5.3.0");
        if let Some(tag) = tasmota_platform_tag() {
            assert_eq!(
                esptool.download_url().unwrap(),
                Esptool::release_url("5.3.0", tag)
            );
        }
    }
}
