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
//! 1. The version is taken from the pioarduino `tool-esptoolpy` metadata URL
//!    (`.../esptoolpy-v5.3.0.zip` → `5.3.0`) — see `extract_esptool_version`.
//! 2. The host `(OS, ARCH)` maps to a tasmota platform tag
//!    (`linux-amd64`, `macos-arm64`, `windows-amd64`, …). An unsupported host
//!    yields an error, and the caller falls back to an `esptool` on PATH.
//! 3. `https://github.com/tasmota/esptool/releases/download/v{version}/esptool-{platform}.zip`
//!    is downloaded + extracted via the shared [`PackageBase::staged_install`]
//!    pattern, and the `esptool` executable is located inside it.

use std::path::Path;

use fbuild_core::{path::NormalizedPath, subprocess::run_command, FbuildError, Result};

use crate::{CacheSubdir, PackageBase};

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
    /// embedded in the URL filename is used.
    pub fn from_metadata_url(project_dir: &Path, metadata_url: &str) -> Self {
        Self {
            project_dir: NormalizedPath::from(project_dir),
            version: extract_esptool_version(metadata_url),
        }
    }

    /// Ensure the standalone esptool binary is installed and return its path.
    /// The caller runs it directly as `<bin> --chip <chip> elf2image …`.
    ///
    /// Cache-aware: installs via the shared [`PackageBase::staged_install`]
    /// pattern, so a warm cache costs no network I/O. Returns an error on an
    /// unsupported host or a missing binary, so the caller can fall back to an
    /// `esptool` on PATH.
    pub async fn ensure_installed(&self) -> Result<NormalizedPath> {
        let platform = tasmota_platform_tag().ok_or_else(|| {
            FbuildError::PackageError(format!(
                "no prebuilt esptool binary for {}/{}",
                std::env::consts::OS,
                std::env::consts::ARCH
            ))
        })?;
        let url = format!(
            "https://github.com/tasmota/esptool/releases/download/v{}/esptool-{}.zip",
            self.version, platform
        );

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
    std::fs::remove_dir_all(install_path).map_err(|e| {
        FbuildError::PackageError(format!(
            "failed to remove cached esptool install {}: {}",
            install_path.display(),
            e
        ))
    })
}

/// Verify that the standalone executable can actually be launched.
///
/// `Path::is_file` is insufficient: a restored cache can retain a regular
/// file whose interpreter or dynamic loader is unavailable, which surfaces as
/// `ENOENT` only when the later `elf2image` command is spawned.
async fn verify_esptool_binary(bin: &Path) -> Result<()> {
    let bin_arg = bin.to_string_lossy();
    let output = run_command(
        &[bin_arg.as_ref(), "--version"],
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
        Err(FbuildError::PackageError(format!(
            "cached esptool executable {} exited with status {}",
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

/// Extract a version string (e.g. `"5.3.0"`) from the pioarduino
/// `tool-esptoolpy` metadata URL. The URL looks like
/// `.../releases/download/0.0.1/esptoolpy-v5.3.0.zip`, where `0.0.1` is the
/// registry release tag and the real esptool version lives in the filename —
/// so we parse the **filename**, not an earlier path segment. Falls back to
/// `"unknown"` (which then fails to resolve a release and triggers the PATH
/// fallback) rather than silently using the wrong version.
fn extract_esptool_version(url: &str) -> String {
    let filename = url.rsplit('/').next().unwrap_or(url);
    let stem = filename.trim_end_matches(".zip");
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
                return cand.to_string();
            }
        } else {
            i += 1;
        }
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
    async fn verify_accepts_runnable_standalone_binary() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::TempDir::new().unwrap();
        let bin = tmp.path().join(esptool_bin_name());
        std::fs::write(&bin, b"#!/bin/sh\nexit 0\n").unwrap();
        let mut permissions = std::fs::metadata(&bin).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&bin, permissions).unwrap();

        verify_esptool_binary(&bin).await.unwrap();
    }
}
