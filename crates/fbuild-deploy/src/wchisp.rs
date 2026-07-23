//! CH32V USB-ISP deployment through the external `wchisp` flasher.

use std::path::{Path, PathBuf};
use std::time::Duration;

use fbuild_core::subprocess::run_command;
use fbuild_core::{FbuildError, Result};

const WCHISP_TIMEOUT: Duration = Duration::from_secs(120);
const WCHISP_RELEASE_TAG: &str = "v0.3.0";
const WCHISP_RELEASE_BASE: &str = "https://github.com/ch32-rs/wchisp/releases/download";

#[derive(Debug, Clone, Copy)]
struct WchispAsset {
    name: &'static str,
    sha256: &'static str,
}

fn release_asset() -> Result<WchispAsset> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Ok(WchispAsset {
            name: "wchisp-v0.3.0-win-x64.zip",
            sha256: "eba605bbc62f217f6454e7236d04ef1b8a6b4396dd7ce8dc26fc83016213c3aa",
        }),
        ("linux", "x86_64") => Ok(WchispAsset {
            name: "wchisp-v0.3.0-linux-x64.tar.gz",
            sha256: "67e3d4eb0ffd3cc610d8927e3c3f452e2110531a3f14405dcaef87df219f200d",
        }),
        ("linux", "aarch64") => Ok(WchispAsset {
            name: "wchisp-v0.3.0-linux-aarch64.tar.gz",
            sha256: "3d7477c05c65f69091d041623a06c558c549dc27bfe2a043d9325f310f2ee40f",
        }),
        ("macos", "aarch64") => Ok(WchispAsset {
            name: "wchisp-v0.3.0-macos-arm64.tar.gz",
            sha256: "a17dd422f7697bfe35c7c837c16bf99a7193300bcdc276c1253332b3a023e936",
        }),
        ("macos", "x86_64") => Ok(WchispAsset {
            name: "wchisp-v0.3.0-macos-x64.tar.gz",
            sha256: "ebbf46b0c64bb356cd58da2683c8809c50bdfe2181969f544933d24c8846f608",
        }),
        (os, arch) => Err(FbuildError::PackageError(format!(
            "wchisp {WCHISP_RELEASE_TAG} has no pinned asset for {os}/{arch}; set FBUILD_WCHISP_PATH"
        ))),
    }
}

fn managed_wchisp_path() -> Result<PathBuf> {
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
        .join("wchisp")
        .join(if cfg!(windows) {
            "wchisp.exe"
        } else {
            "wchisp"
        }))
}

async fn ensure_wchisp_installed() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("FBUILD_WCHISP_PATH").map(PathBuf::from) {
        if path.is_file() {
            return Ok(path);
        }
        return Err(FbuildError::PackageError(format!(
            "FBUILD_WCHISP_PATH does not name a file: {}",
            path.display()
        )));
    }
    let dest = managed_wchisp_path()?;
    if dest.is_file() {
        return Ok(dest);
    }
    let asset = release_asset()?;
    let staging = fbuild_paths::temp_subdir("wchisp-install");
    fbuild_core::fs::create_dir_all(&staging).await?;
    let url = format!("{WCHISP_RELEASE_BASE}/{WCHISP_RELEASE_TAG}/{}", asset.name);
    let archive = fbuild_packages::downloader::download_file(&url, &staging).await?;
    fbuild_packages::downloader::verify_checksum_async(&archive, asset.sha256).await?;
    let dest_clone = dest.clone();
    let staging_clone = staging.clone();
    tokio::task::spawn_blocking(move || extract_wchisp(&archive, &staging_clone, &dest_clone))
        .await
        .map_err(|e| FbuildError::PackageError(format!("wchisp install task failed: {e}")))??;
    let _ = fbuild_core::fs::remove_dir_all(&staging).await;
    Ok(dest)
}

fn extract_wchisp(archive: &Path, staging: &Path, dest: &Path) -> Result<()> {
    let extract_dir = staging.join("extract");
    std::fs::create_dir_all(&extract_dir)
        .map_err(|e| FbuildError::PackageError(format!("create wchisp extract dir: {e}")))?;
    if archive.extension().is_some_and(|ext| ext == "zip") {
        let file = std::fs::File::open(archive)
            .map_err(|e| FbuildError::PackageError(format!("open wchisp archive: {e}")))?;
        zip::ZipArchive::new(file)
            .map_err(|e| FbuildError::PackageError(format!("read wchisp archive: {e}")))?
            .extract(&extract_dir)
            .map_err(|e| FbuildError::PackageError(format!("extract wchisp archive: {e}")))?;
    } else {
        let file = std::fs::File::open(archive)
            .map_err(|e| FbuildError::PackageError(format!("open wchisp archive: {e}")))?;
        tar::Archive::new(flate2::read::GzDecoder::new(file))
            .unpack(&extract_dir)
            .map_err(|e| FbuildError::PackageError(format!("extract wchisp archive: {e}")))?;
    }
    let name = if cfg!(windows) {
        "wchisp.exe"
    } else {
        "wchisp"
    };
    let binary = find_file(&extract_dir, name)
        .ok_or_else(|| FbuildError::PackageError(format!("wchisp archive lacks {name}")))?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| FbuildError::PackageError(format!("create wchisp install dir: {e}")))?;
    }
    std::fs::copy(binary, dest)
        .map_err(|e| FbuildError::PackageError(format!("install wchisp: {e}")))?;
    #[cfg(unix)]
    std::fs::set_permissions(dest, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .map_err(|e| FbuildError::PackageError(format!("make wchisp executable: {e}")))?;
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

/// Factory USB-ISP support excludes V003/V006, which have no USB peripheral.
pub fn supports_mcu(mcu: &str) -> bool {
    let upper = mcu.to_ascii_uppercase();
    upper.starts_with("CH32V103")
        || upper.starts_with("CH32V203")
        || upper.starts_with("CH32V208")
        || upper.starts_with("CH32V303")
        || upper.starts_with("CH32V307")
        || upper.starts_with("CH32X035")
        || upper.starts_with("CH32L103")
}

#[derive(Debug, Clone)]
pub struct WchispDeployer {
    executable: PathBuf,
}

impl WchispDeployer {
    pub fn new() -> Self {
        Self {
            executable: std::env::var_os("FBUILD_WCHISP_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    PathBuf::from(if cfg!(windows) {
                        "wchisp.exe"
                    } else {
                        "wchisp"
                    })
                }),
        }
    }
    pub fn flash_argv(&self, firmware_path: &Path) -> Vec<String> {
        vec![
            self.executable.to_string_lossy().into_owned(),
            "flash".into(),
            firmware_path.to_string_lossy().into_owned(),
        ]
    }
}
impl Default for WchispDeployer {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl crate::Deployer for WchispDeployer {
    async fn deploy(
        &self,
        _project_dir: &Path,
        _env_name: &str,
        firmware_path: &Path,
        _port: Option<&str>,
    ) -> Result<crate::DeploymentResult> {
        let executable = ensure_wchisp_installed().await?;
        let argv = [
            executable.to_string_lossy().into_owned(),
            "flash".to_string(),
            firmware_path.to_string_lossy().into_owned(),
        ];
        let refs = argv.iter().map(String::as_str).collect::<Vec<_>>();
        let result = run_command(&refs, None, None, Some(WCHISP_TIMEOUT))
            .await
            .map_err(|e| FbuildError::DeployFailed(format!("failed to run wchisp: {e}")))?;
        let success = result.success();
        let message = if success {
            "firmware flashed through wchisp".to_string()
        } else {
            format!(
                "wchisp failed (exit code {}). If no USB-ISP device was found, hold BOOT0/Download while resetting; on Windows install a WinUSB/WCH USB driver for the WCH USB-ISP bootloader device (identify it with `fbuild port scan`).{}",
                result.exit_code,
                if result.stderr.is_empty() {
                    ""
                } else {
                    " See the flasher diagnostics above."
                }
            )
        };
        Ok(crate::DeploymentResult {
            success,
            message,
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
    fn supports_usb_isp_families_but_not_v00x() {
        assert!(supports_mcu("CH32V203C8T6"));
        assert!(supports_mcu("CH32X035R8T6"));
        assert!(!supports_mcu("CH32V003F4P6"));
        assert!(!supports_mcu("CH32V006J8M6"));
    }
    #[test]
    fn flash_argv_uses_wchisp_flash_command() {
        let deployer = WchispDeployer {
            executable: PathBuf::from("wchisp"),
        };
        assert_eq!(
            deployer.flash_argv(Path::new("build/firmware.bin")),
            ["wchisp", "flash", "build/firmware.bin"]
        );
    }
}
