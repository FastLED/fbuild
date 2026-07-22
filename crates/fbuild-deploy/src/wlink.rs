//! CH32V deployment through the `wlink` WCH-LinkE flasher.
//!
//! `wlink` is intentionally invoked as an external executable: the tool is
//! released independently and supports the complete CH32V family without
//! linking its implementation into fbuild. `FBUILD_WLINK_PATH` can point at a
//! pinned or locally-built binary; otherwise `wlink` is resolved from PATH.

use std::path::{Path, PathBuf};
use std::time::Duration;

use fbuild_core::subprocess::run_command;
use fbuild_core::{FbuildError, Result};

const WLINK_TIMEOUT: Duration = Duration::from_secs(120);
const WLINK_RELEASE_TAG: &str = "v0.1.2";
const WLINK_RELEASE_BASE: &str = "https://github.com/ch32-rs/wlink/releases/download";

#[derive(Debug, Clone, Copy)]
struct WlinkAsset {
    name: &'static str,
    sha256: &'static str,
}

fn release_asset() -> Result<WlinkAsset> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Ok(WlinkAsset {
            name: "wlink-v0.1.2-win-x64.zip",
            sha256: "59b3989137a9d22c9c1e8c04fd9371af3f54fa43b4cb63c59d6fb4286a34c78a",
        }),
        ("linux", "x86_64") => Ok(WlinkAsset {
            name: "wlink-v0.1.2-linux-x64.tar.gz",
            sha256: "f8f1fba2436694116fe2cf16b1572e92d116c4acd921bf12fbc0ca5bf63824bf",
        }),
        ("macos", "aarch64") => Ok(WlinkAsset {
            name: "wlink-v0.1.2-macos-arm64.tar.gz",
            sha256: "49164d236346e4c294935412a072040eac8faaeb5f097952846807f7dc0fbf8c",
        }),
        (os, arch) => Err(FbuildError::PackageError(format!(
            "wlink v{WLINK_RELEASE_TAG} has no pinned asset for {os}/{arch}; set FBUILD_WLINK_PATH"
        ))),
    }
}

fn managed_wlink_path() -> Result<PathBuf> {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .ok_or_else(|| {
            FbuildError::PackageError("could not determine home directory".to_string())
        })?;
    let mode = if std::env::var_os("FBUILD_DEV_MODE").is_some() {
        "dev"
    } else {
        "prod"
    };
    Ok(home
        .join(".fbuild")
        .join(mode)
        .join("tools")
        .join("wlink")
        .join(if cfg!(windows) { "wlink.exe" } else { "wlink" }))
}

async fn ensure_wlink_installed() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("FBUILD_WLINK_PATH").map(PathBuf::from) {
        if path.is_file() {
            return Ok(path);
        }
        return Err(FbuildError::PackageError(format!(
            "FBUILD_WLINK_PATH does not name a file: {}",
            path.display()
        )));
    }
    let dest = managed_wlink_path()?;
    if dest.is_file() {
        return Ok(dest);
    }
    let asset = release_asset()?;
    let staging = fbuild_paths::temp_subdir("wlink-install");
    fbuild_core::fs::create_dir_all(&staging).await?;
    let url = format!("{WLINK_RELEASE_BASE}/{WLINK_RELEASE_TAG}/{}", asset.name);
    let archive = fbuild_packages::downloader::download_file(&url, &staging).await?;
    fbuild_packages::downloader::verify_checksum_async(&archive, asset.sha256).await?;
    let dest_clone = dest.clone();
    let staging_clone = staging.clone();
    tokio::task::spawn_blocking(move || extract_wlink(&archive, &staging_clone, &dest_clone))
        .await
        .map_err(|e| FbuildError::PackageError(format!("wlink install task failed: {e}")))??;
    let _ = fbuild_core::fs::remove_dir_all(&staging).await;
    Ok(dest)
}

fn extract_wlink(archive: &Path, staging: &Path, dest: &Path) -> Result<()> {
    let extract_dir = staging.join("extract");
    std::fs::create_dir_all(&extract_dir)
        .map_err(|e| FbuildError::PackageError(format!("create wlink extract dir: {e}")))?;
    if archive.extension().is_some_and(|ext| ext == "zip") {
        let file = std::fs::File::open(archive)
            .map_err(|e| FbuildError::PackageError(format!("open wlink archive: {e}")))?;
        let mut zip = zip::ZipArchive::new(file)
            .map_err(|e| FbuildError::PackageError(format!("read wlink archive: {e}")))?;
        zip.extract(&extract_dir)
            .map_err(|e| FbuildError::PackageError(format!("extract wlink archive: {e}")))?;
    } else {
        let file = std::fs::File::open(archive)
            .map_err(|e| FbuildError::PackageError(format!("open wlink archive: {e}")))?;
        let decoder = flate2::read::GzDecoder::new(file);
        let mut tar = tar::Archive::new(decoder);
        tar.unpack(&extract_dir)
            .map_err(|e| FbuildError::PackageError(format!("extract wlink archive: {e}")))?;
    }
    let binary_name = if cfg!(windows) { "wlink.exe" } else { "wlink" };
    let binary = find_file(&extract_dir, binary_name)
        .ok_or_else(|| FbuildError::PackageError(format!("wlink archive lacks {binary_name}")))?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| FbuildError::PackageError(format!("create wlink install dir: {e}")))?;
    }
    std::fs::copy(binary, dest)
        .map_err(|e| FbuildError::PackageError(format!("install wlink: {e}")))?;
    #[cfg(unix)]
    std::fs::set_permissions(dest, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .map_err(|e| FbuildError::PackageError(format!("make wlink executable: {e}")))?;
    Ok(())
}

fn find_file(root: &Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        let path = entry.path();
        if path.is_file() && path.file_name().and_then(|n| n.to_str()) == Some(name) {
            return Some(path);
        }
        if path.is_dir() {
            if let Some(found) = find_file(&path, name) {
                return Some(found);
            }
        }
    }
    None
}

#[derive(Debug, Clone)]
pub struct WlinkDeployer {
    executable: PathBuf,
}

impl WlinkDeployer {
    pub fn new() -> Self {
        let executable = std::env::var_os("FBUILD_WLINK_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(if cfg!(windows) { "wlink.exe" } else { "wlink" }));
        Self { executable }
    }

    pub fn flash_argv(&self, firmware_path: &Path) -> Vec<String> {
        vec![
            self.executable.to_string_lossy().into_owned(),
            "flash".to_string(),
            firmware_path.to_string_lossy().into_owned(),
        ]
    }
}

impl Default for WlinkDeployer {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl crate::Deployer for WlinkDeployer {
    async fn deploy(
        &self,
        _project_dir: &Path,
        _env_name: &str,
        firmware_path: &Path,
        _port: Option<&str>,
    ) -> Result<crate::DeploymentResult> {
        let executable = ensure_wlink_installed().await?;
        let argv = [
            executable.to_string_lossy().into_owned(),
            "flash".to_string(),
            firmware_path.to_string_lossy().into_owned(),
        ];
        let refs = argv.iter().map(String::as_str).collect::<Vec<_>>();
        let result = run_command(&refs, None, None, Some(WLINK_TIMEOUT))
            .await
            .map_err(|e| FbuildError::DeployFailed(format!("failed to run wlink: {e}")))?;
        let success = result.success();
        Ok(crate::DeploymentResult {
            success,
            message: if success {
                "firmware flashed through wlink".to_string()
            } else {
                format!("wlink failed (exit code {})", result.exit_code)
            },
            port: None,
            stdout: result.stdout,
            stderr: result.stderr,
            outcome: crate::DeployOutcome::FullFlash,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flash_argv_uses_wlink_flash_command() {
        let deployer = WlinkDeployer {
            executable: PathBuf::from("wlink"),
        };
        assert_eq!(
            deployer.flash_argv(Path::new("build/firmware.bin")),
            ["wlink", "flash", "build/firmware.bin"]
        );
    }
}
