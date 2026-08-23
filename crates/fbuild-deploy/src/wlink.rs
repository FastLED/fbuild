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
    match (
        fbuild_core::platform::host::os_name(),
        fbuild_core::platform::host::arch_name(),
    ) {
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
    let tools = fbuild_paths::try_get_tools_dir().ok_or_else(|| {
        FbuildError::PackageError("could not determine home directory".to_string())
    })?;
    Ok(tools
        .join("wlink")
        .join(fbuild_core::platform::executable::native_name("wlink")))
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
    let binary_name = fbuild_core::platform::executable::native_name("wlink");
    let binary = find_file(&extract_dir, &binary_name)
        .ok_or_else(|| FbuildError::PackageError(format!("wlink archive lacks {binary_name}")))?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| FbuildError::PackageError(format!("create wlink install dir: {e}")))?;
    }
    std::fs::copy(binary, dest)
        .map_err(|e| FbuildError::PackageError(format!("install wlink: {e}")))?;
    fbuild_core::platform::fs::set_executable(dest)
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
            .unwrap_or_else(|| {
                PathBuf::from(fbuild_core::platform::executable::native_name("wlink"))
            });
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

    /// Exercises the pinned-asset download + SHA-256 verification +
    /// extraction path end to end. Needs the network but **no hardware**,
    /// so it catches a rotted release URL or a stale checksum without a
    /// probe on the bench.
    ///
    /// ```text
    /// soldr cargo test -p fbuild-deploy wlink::tests::try_install_wlink_from_pinned_release -- --ignored --nocapture
    /// ```
    #[tokio::test]
    #[ignore = "downloads the pinned wlink release from GitHub (~2 MB)"]
    async fn try_install_wlink_from_pinned_release() {
        let asset = release_asset().expect("host must have a pinned wlink asset");
        eprintln!("installing {} ({})", asset.name, WLINK_RELEASE_TAG);

        let path = ensure_wlink_installed()
            .await
            .expect("wlink install must succeed: download + checksum + extract");

        assert!(path.is_file(), "wlink binary missing at {}", path.display());

        let argv = [path.to_string_lossy().into_owned(), "--version".to_string()];
        let refs = argv.iter().map(String::as_str).collect::<Vec<_>>();
        let out = run_command(&refs, None, None, Some(WLINK_TIMEOUT))
            .await
            .expect("installed wlink must be executable");
        assert!(
            out.success(),
            "wlink --version failed (exit {}): {}",
            out.exit_code,
            out.stderr
        );
        eprintln!("wlink --version: {}", out.stdout.trim());
    }

    /// Phase 0 of the FastLED/fbuild#1208 bring-up: prove the probe sees
    /// the part before trusting anything downstream.
    ///
    /// Requires a WCH-LinkE in **RV mode** (`1A86:8010`) with SWIO/GND/3V3
    /// wired to a CH32V003. On Windows the probe interface must have WinUSB
    /// bound via Zadig or libusb cannot claim it.
    ///
    /// Out of DAP mode (`1A86:8012`): `wlink mode-switch --rv` if `wlink`
    /// can already reach the probe, otherwise hold the button while
    /// plugging in. `wlink set-power enable3v3` powers the target off the
    /// probe if it has no supply of its own.
    ///
    /// Presence is checked with `status`, deliberately not `list` — with no
    /// probe attached `wlink list` exits **0** and prints nothing, so it
    /// cannot distinguish "no probe" from success. `status` exits 1.
    ///
    /// ```text
    /// soldr cargo test -p fbuild-deploy wlink::tests::try_wlink_status_detects_ch32v003 -- --ignored --nocapture
    /// ```
    #[tokio::test]
    #[ignore = "requires CH32V003 + WCH-LinkE probe in RV mode (1A86:8010)"]
    async fn try_wlink_status_detects_ch32v003() {
        let executable = ensure_wlink_installed()
            .await
            .expect("wlink install must succeed");
        let argv = [
            executable.to_string_lossy().into_owned(),
            "status".to_string(),
        ];
        let refs = argv.iter().map(String::as_str).collect::<Vec<_>>();
        let out = run_command(&refs, None, None, Some(WLINK_TIMEOUT))
            .await
            .expect("failed to run wlink status");

        eprintln!("--- wlink status ---\n{}\n{}", out.stdout, out.stderr);
        assert!(
            out.success(),
            "wlink status failed (exit {}). Probe not in RV mode, WinUSB not bound, \
             or target not wired. See crates/fbuild-build-mcu/src/ch32v/README.md.\n{}",
            out.exit_code,
            out.stderr
        );

        let combined = format!("{}{}", out.stdout, out.stderr).to_ascii_lowercase();
        assert!(
            combined.contains("ch32v003") || combined.contains("chip id"),
            "wlink status did not report a chip id; probe may be connected \
             without a target attached. Output:\n{combined}"
        );
    }

    /// Phase 2 of the bring-up — the actual flash. Point `CH32V003_FIRMWARE`
    /// at a `.bin` built by
    /// `fbuild build tests/platform/ch32v003 -e ch32v003`.
    ///
    /// A passing flash is **not** proof of bring-up: confirm the blink on a
    /// scope or LED afterwards. This test only asserts that `wlink` accepted
    /// and wrote the image.
    ///
    /// ```text
    /// CH32V003_FIRMWARE=C:\path\to\firmware.bin \
    ///   soldr cargo test -p fbuild-deploy wlink::tests::try_flash_real_ch32v003 -- --ignored --nocapture
    /// ```
    #[tokio::test]
    #[ignore = "requires CH32V003 + WCH-LinkE probe — set CH32V003_FIRMWARE"]
    async fn try_flash_real_ch32v003() {
        use crate::Deployer;

        let firmware = std::env::var("CH32V003_FIRMWARE")
            .expect("set CH32V003_FIRMWARE to a .bin built for ch32v003");
        let firmware = PathBuf::from(firmware);
        assert!(
            firmware.is_file(),
            "CH32V003_FIRMWARE does not exist: {}",
            firmware.display()
        );

        // The part has 16 KB of flash; a larger image cannot be a valid
        // V003 build and would fail confusingly deeper in wlink.
        let size = std::fs::metadata(&firmware).expect("stat firmware").len();
        assert!(
            size <= 16 * 1024,
            "firmware is {size} bytes, over the CH32V003 16384-byte flash budget"
        );
        eprintln!("flashing {} ({size} bytes)", firmware.display());

        let result = WlinkDeployer::new()
            .deploy(Path::new("."), "ch32v003", &firmware, None)
            .await
            .expect("deploy must not error");

        eprintln!("--- wlink flash ---\n{}\n{}", result.stdout, result.stderr);
        assert!(result.success, "flash failed: {}", result.message);
        eprintln!("flashed OK — now verify the blink on a scope or LED");
    }

    /// CH32V003 code-flash is aliased at the CPU's boot address.
    const CH32V003_FLASH_BASE: u32 = 0x0800_0000;

    async fn run_wlink(args: &[&str]) -> fbuild_core::subprocess::ToolOutput {
        let executable = ensure_wlink_installed()
            .await
            .expect("wlink install must succeed");
        let mut argv = vec![executable.to_string_lossy().into_owned()];
        argv.extend(args.iter().map(|a| (*a).to_string()));
        let refs = argv.iter().map(String::as_str).collect::<Vec<_>>();
        run_command(&refs, None, None, Some(WLINK_TIMEOUT))
            .await
            .unwrap_or_else(|e| panic!("failed to run wlink {args:?}: {e}"))
    }

    /// Read the image back off the chip and byte-compare it.
    ///
    /// `wlink flash` reporting success only means the tool accepted the
    /// write. This proves the bytes actually landed — it catches a partial
    /// write, a wrong base address, or write-protected flash, none of which
    /// the exit code distinguishes. Run after `try_flash_real_ch32v003`.
    ///
    /// Still short of the Phase 2 milestone: verified flash contents are not
    /// a running program. Observe the blink.
    ///
    /// ```text
    /// CH32V003_FIRMWARE=C:\path\to\firmware.bin \
    ///   soldr cargo test -p fbuild-deploy wlink::tests::try_verify_flash_readback_ch32v003 -- --ignored --nocapture
    /// ```
    #[tokio::test]
    #[ignore = "requires CH32V003 + WCH-LinkE probe — set CH32V003_FIRMWARE"]
    async fn try_verify_flash_readback_ch32v003() {
        let firmware = PathBuf::from(
            std::env::var("CH32V003_FIRMWARE")
                .expect("set CH32V003_FIRMWARE to the .bin that was flashed"),
        );
        let expected = std::fs::read(&firmware).expect("read firmware image");
        assert!(!expected.is_empty(), "firmware image is empty");

        let dump_dir = fbuild_paths::temp_subdir("ch32v003-readback");
        fbuild_core::fs::create_dir_all(&dump_dir)
            .await
            .expect("create dump dir");
        let dump_path = dump_dir.join("readback.bin");

        // `dump` rounds the length up to the next multiple of 4.
        let length = expected.len().div_ceil(4) * 4;
        let out = run_wlink(&[
            "dump",
            &format!("0x{CH32V003_FLASH_BASE:08x}"),
            &length.to_string(),
            "-o",
            &dump_path.to_string_lossy(),
        ])
        .await;
        assert!(
            out.success(),
            "wlink dump failed (exit {}): {}",
            out.exit_code,
            out.stderr
        );

        let actual = std::fs::read(&dump_path).expect("read dumped flash");
        assert!(
            actual.len() >= expected.len(),
            "dump returned {} bytes, expected at least {}",
            actual.len(),
            expected.len()
        );

        if let Some(offset) = (0..expected.len()).find(|&i| actual[i] != expected[i]) {
            panic!(
                "flash readback differs from image at offset 0x{offset:04x} \
                 (chip 0x{:02x} != image 0x{:02x}); {} of {} bytes matched. \
                 A successful `wlink flash` exit code does not imply the write landed.",
                actual[offset],
                expected[offset],
                offset,
                expected.len()
            );
        }
        eprintln!(
            "readback verified: {} bytes on-chip match {}",
            expected.len(),
            firmware.display()
        );
    }

    /// Prove the core is *executing*, not just programmed.
    ///
    /// Resumes the MCU and samples the program counter twice. A PC that
    /// advances between samples means the CPU is retiring instructions —
    /// the strongest evidence available without physical instrumentation,
    /// and it distinguishes "flashed but halted / stuck in a fault loop"
    /// from "running", which readback alone cannot.
    ///
    /// This is a **proxy**, not the Phase 2 milestone. It cannot tell you
    /// the GPIO is toggling at the right rate; only a scope or LED can.
    ///
    /// ```text
    /// soldr cargo test -p fbuild-deploy wlink::tests::try_ch32v003_core_is_executing -- --ignored --nocapture
    /// ```
    #[tokio::test]
    #[ignore = "requires CH32V003 + WCH-LinkE probe in RV mode (1A86:8010)"]
    async fn try_ch32v003_core_is_executing() {
        let resume = run_wlink(&["resume"]).await;
        assert!(
            resume.success(),
            "wlink resume failed (exit {}): {}",
            resume.exit_code,
            resume.stderr
        );

        let sample = || async {
            let out = run_wlink(&["regs"]).await;
            assert!(
                out.success(),
                "wlink regs failed (exit {}): {}",
                out.exit_code,
                out.stderr
            );
            format!("{}{}", out.stdout, out.stderr)
        };

        let first = sample().await;
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        let second = sample().await;

        eprintln!("--- regs sample 1 ---\n{first}\n--- regs sample 2 ---\n{second}");
        assert_ne!(
            first.trim(),
            second.trim(),
            "register state identical across a 250ms gap — the core looks halted \
             or stuck. A flashed-but-not-running chip reaches this state, so treat \
             it as a bring-up failure rather than a flaky read."
        );
        eprintln!(
            "core is executing (register state advanced) — \
             still confirm the blink rate on a scope or LED"
        );
    }
}
