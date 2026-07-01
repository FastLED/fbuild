//! NXP LPC8xx deployer using `lpc21isp`.
//!
//! Flashes firmware.hex to LPC8xx boards via ISP-over-UART. The on-die ROM
//! boot loader handles the protocol; the host just spawns `lpc21isp` with
//! the firmware path, port, baud rate, and crystal frequency.
//!
//! ## Why lpc21isp first (and not pyOCD / CMSIS-DAP)
//!
//! On the LPC845-BRK the on-board debug probe presents a *composite* USB
//! device: CMSIS-DAP (HID) + Mass-Storage + CDC (the application's VCOM).
//! pyOCD's SWD flash path opens the HID interface, and on Windows it
//! leaves the CDC sibling in error 31 (requires a physical USB replug to
//! recover) — see [FastLED/fbuild#565]. That breaks the flash-then-monitor
//! cycle every FastLED test harness depends on (AutoResearch, JSON-RPC
//! bring-up, etc.).
//!
//! lpc21isp uses ISP-over-UART. It never opens the composite-device HID
//! interface, so the CDC sibling stays available across the flash. That's
//! the primary path. SWD remains a future addition for boards without an
//! exposed UART (rare on LPC8xx) — tracked separately.
//!
//! ## Reset / ISP-mode entry
//!
//! lpc21isp's `-control` flag drives DTR/RTS to put the chip into ISP
//! mode before flashing and back into run-mode afterward. The LPC845-BRK
//! wiring matches `-control` semantics out of the box. Boards without
//! auto-reset wiring need the user to hold ISP+RESET manually; the same
//! `-control` argument is harmless on those.
//!
//! [FastLED/fbuild#565]: https://github.com/FastLED/fbuild/issues/565

use std::path::{Path, PathBuf};

use fbuild_core::subprocess::run_command;
use fbuild_core::Result;

use crate::{DeployOutcome, Deployer, DeploymentResult};

/// Env var pointing directly at the `lpc21isp` binary. Bypasses every
/// path search below when set — useful for CI / dev overrides where the
/// tool lives in an unusual location.
///
/// FastLED/fbuild#921: the primary escape hatch until fbuild auto-fetches
/// lpc21isp itself.
pub const LPC21ISP_PATH_ENV_VAR: &str = "FBUILD_LPC21ISP_PATH";

/// Resolve `$HOME` (`$USERPROFILE` on Windows). Kept as a small local
/// helper so this crate does not gain a `dirs` / `home` dependency for
/// one call site — mirrors the same pattern in `fbuild-paths`.
fn home_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

/// Resolve where `lpc21isp` lives on this system.
///
/// Search order (first hit wins):
///
/// 1. `FBUILD_LPC21ISP_PATH` env var.
/// 2. `~/.fbuild/tools/lpc21isp[.exe]` — fbuild's managed tools dir. Also
///    honors `FBUILD_DEV_MODE=1` → `~/.fbuild/dev/tools/…`.
/// 3. `C:\tools\lpc21isp\lpc21isp.exe` on Windows — a widely-followed
///    convention that this repo's fresh-install docs point at (matches
///    the empty placeholder dir already present on maintainer boxes).
/// 4. `lpc21isp` (or `lpc21isp.exe`) on `PATH`.
///
/// Returns `None` if no candidate is found — the deployer converts that
/// into a clear "how to install lpc21isp" diagnostic instead of blowing
/// up mid-flash. FastLED/fbuild#921.
pub fn find_lpc21isp() -> Option<PathBuf> {
    if let Some(env_hit) = std::env::var_os(LPC21ISP_PATH_ENV_VAR) {
        let p = PathBuf::from(env_hit);
        if p.is_file() {
            return Some(p);
        }
    }

    let exe = if cfg!(windows) {
        "lpc21isp.exe"
    } else {
        "lpc21isp"
    };

    // fbuild-managed tools dir. Mirrors the `~/.fbuild/{prod|dev}/`
    // isolation the rest of fbuild-paths applies.
    if let Some(home) = home_dir() {
        let mode = if std::env::var_os("FBUILD_DEV_MODE").is_some() {
            "dev"
        } else {
            "prod"
        };
        let managed = home.join(".fbuild").join(mode).join("tools").join(exe);
        if managed.is_file() {
            return Some(managed);
        }
        // Also check the mode-less legacy path some maintainer setups use.
        let legacy = home.join(".fbuild").join("tools").join(exe);
        if legacy.is_file() {
            return Some(legacy);
        }
    }

    // Windows convention: `C:\tools\lpc21isp\lpc21isp.exe`. Matches the
    // maintainer-box layout referenced in FastLED/fbuild#921.
    if cfg!(windows) {
        let tools_dir = PathBuf::from("C:\\tools\\lpc21isp").join(exe);
        if tools_dir.is_file() {
            return Some(tools_dir);
        }
    }

    // PATH lookup — via `which` when the crate is available on the host
    // toolchain, otherwise a manual walk of `$PATH`.
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(exe);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    None
}

/// Build the "install lpc21isp" hint that surfaces on the failing
/// deploy path. Kept as a standalone function so the test module can
/// assert the exact URLs / paths without shelling out.
pub(crate) fn lpc21isp_install_hint() -> String {
    format!(
        "lpc21isp not found on PATH or in any fbuild-managed tools dir.\n\
         \n\
         Install one of the following, then retry:\n\
         \n\
           • Windows prebuilt: fetch lpc21isp.exe from\n\
             https://sourceforge.net/projects/lpc21isp/files/ and drop\n\
             it in C:\\tools\\lpc21isp\\lpc21isp.exe (or set\n\
             {env}=<full path to lpc21isp.exe>).\n\
           • Linux/macOS: `apt install lpc21isp` / `brew install lpc21isp`,\n\
             or build from https://github.com/capiman/lpc21isp source.\n\
         \n\
         Once installed, verify with: `lpc21isp` (should print usage).\n\
         Tracked under FastLED/fbuild#921.",
        env = LPC21ISP_PATH_ENV_VAR
    )
}

/// lpc21isp deploy parameters sourced from MCU config JSON.
///
/// All LPC845 / LPC804 boards (including the LPC845-BRK and the
/// LPCXpresso variants) use the same on-die ISP ROM, so a single
/// parameter struct covers the family. The crystal frequency in kHz
/// is the only knob that's not in the existing `upload.*` board JSON
/// fields — default 12000 covers every LPC8xx evaluation board NXP
/// ships today.
pub struct Lpc21IspParams {
    /// Default baud rate for lpc21isp. Standard 115200 matches every
    /// LPC8xx board JSON's `upload.speed`. Overridable per-board via
    /// `board.upload_speed`.
    pub default_baud: String,
    /// Crystal frequency in kHz, passed to lpc21isp as its `xtal_kHz`
    /// argument. Used for clock-related verify operations inside the
    /// tool. Default 12000 (12 MHz) matches NXP's LPC845-BRK and the
    /// LPCXpresso845-MAX / LPCXpresso804 reference boards.
    pub xtal_khz: u32,
    /// Hard cap on a single lpc21isp subprocess. Programming the whole
    /// 64 KB of flash via ISP-over-UART at 115200 baud takes ~6 s
    /// worst case; the 60 s default leaves comfortable slack for
    /// auto-baud handshake retries.
    pub timeout_secs: u64,
}

impl Default for Lpc21IspParams {
    fn default() -> Self {
        Self {
            default_baud: "115200".to_string(),
            xtal_khz: 12_000,
            timeout_secs: 60,
        }
    }
}

/// NXP LPC8xx deployer using `lpc21isp` for ISP-over-UART flashing.
pub struct LpcDeployer {
    /// Path to lpc21isp binary (if not in PATH).
    lpc21isp_path: PathBuf,
    /// Baud rate (positional arg after port).
    baud_rate: String,
    /// Crystal frequency in kHz (positional arg after baud).
    xtal_khz: u32,
    /// Deploy timeout in seconds.
    timeout_secs: u64,
    verbose: bool,
}

impl LpcDeployer {
    pub fn new(
        baud_rate: &str,
        xtal_khz: u32,
        timeout_secs: u64,
        lpc21isp_path: Option<PathBuf>,
        verbose: bool,
    ) -> Self {
        Self {
            lpc21isp_path: lpc21isp_path.unwrap_or_else(|| PathBuf::from("lpc21isp")),
            baud_rate: baud_rate.to_string(),
            xtal_khz,
            timeout_secs,
            verbose,
        }
    }

    /// Build an LPC deployer from board config + lpc21isp params.
    ///
    /// Resolves `lpc21isp` through [`find_lpc21isp`] so the deployer runs
    /// against a real binary on disk. If the resolver comes back empty,
    /// the deployer is still constructed (with the literal string
    /// `"lpc21isp"` as its path) so unit tests can build one without
    /// hardware — the `deploy()` call surfaces the actionable "install
    /// lpc21isp" hint at flash time. FastLED/fbuild#921.
    pub fn from_board_config(
        board: &fbuild_config::BoardConfig,
        params: &Lpc21IspParams,
        verbose: bool,
    ) -> Self {
        Self::new(
            board
                .upload_speed
                .as_deref()
                .unwrap_or(&params.default_baud),
            params.xtal_khz,
            params.timeout_secs,
            find_lpc21isp(),
            verbose,
        )
    }

    /// Override the baud rate (e.g. from a CLI `--baud` flag). Mirrors
    /// `AvrDeployer::with_baud_rate` and `Esp32Deployer::with_baud_rate`
    /// so the daemon can apply the user's CLI override on any platform
    /// without branching.
    pub fn with_baud_rate(mut self, baud: &str) -> Self {
        self.baud_rate = baud.to_string();
        self
    }
}

/// Boxed-Deployer constructor used by the daemon dispatch site
/// (`crates/fbuild-daemon/src/handlers/operations/deploy.rs`, the
/// `Platform::NxpLpc` arm). Kept here so the dispatch arm fits in one
/// line — the daemon's deploy.rs lives under a hard 1000-LOC per-file
/// rule and would otherwise need a structural refactor for every new
/// platform.
///
/// The behaviour is the same as inlining all of this in the dispatch
/// arm: build a `BoardConfig` from the env's overrides (defaulting to
/// `lpc845` when no board id is specified), construct an `LpcDeployer`
/// with the family defaults from `Lpc21IspParams::default()`, then
/// apply the CLI's `--baud` override if one was passed.
pub fn dispatch_box(
    board_id: &str,
    board_overrides: &std::collections::HashMap<String, String>,
    project_path: &Path,
    baud_override: Option<u32>,
) -> Box<dyn Deployer> {
    let board_config = fbuild_config::BoardConfig::from_board_id_or_default(
        board_id,
        "lpc845",
        board_overrides,
        Some(project_path),
    );
    let params = Lpc21IspParams::default();
    let deployer = LpcDeployer::from_board_config(&board_config, &params, false);
    let deployer = match baud_override {
        Some(b) => deployer.with_baud_rate(&b.to_string()),
        None => deployer,
    };
    Box::new(deployer)
}

#[async_trait::async_trait]
impl Deployer for LpcDeployer {
    async fn deploy(
        &self,
        _project_dir: &Path,
        _env_name: &str,
        firmware_path: &Path,
        port: Option<&str>,
    ) -> Result<DeploymentResult> {
        let port = port.ok_or_else(|| {
            fbuild_core::FbuildError::DeployFailed(
                "serial port required for LPC deploy (use --port)".to_string(),
            )
        })?;

        // FastLED/fbuild#921: fail fast with an actionable "install
        // lpc21isp" hint BEFORE we try to spawn it. `run_command` would
        // otherwise return a generic ENOENT that hides the fix.
        //
        // If the deployer was constructed with the literal `"lpc21isp"`
        // string (find_lpc21isp() returned None), the file at that
        // relative path does not exist. In every other case
        // (env override, ~/.fbuild/tools/, C:\tools\..., or a resolved
        // PATH hit) `lpc21isp_path` is an absolute path that does exist.
        // Skip the check for shell-name paths so `PATH` lookup at spawn
        // time still works when someone points us at a bare filename.
        if self.lpc21isp_path.components().count() > 1 && !self.lpc21isp_path.is_file() {
            return Err(fbuild_core::FbuildError::DeployFailed(
                lpc21isp_install_hint(),
            ));
        }

        // lpc21isp argv:
        //   lpc21isp [-control] [-wipe] -hex <firmware> <port> <baud> <xtal_kHz>
        //
        // -control : drive DTR/RTS to enter ISP mode and exit it after
        //            flashing. Required for auto-reset boards like the
        //            LPC845-BRK; harmless on boards wired without it
        //            (lpc21isp just no-ops the control lines).
        // -wipe    : full-chip erase before writing. Without this, lpc21isp
        //            only erases the sectors it's about to write, which
        //            leaves stale data in sectors the new firmware happened
        //            to skip. Cheap on 64 KB / 32 KB parts (~250 ms).
        // -hex     : firmware is in Intel HEX format. fbuild's nxplpc
        //            orchestrator emits .hex (see nxplpc/orchestrator.rs).
        let args = [
            self.lpc21isp_path.to_string_lossy().to_string(),
            "-control".to_string(),
            "-wipe".to_string(),
            "-hex".to_string(),
            firmware_path.to_string_lossy().to_string(),
            port.to_string(),
            self.baud_rate.clone(),
            self.xtal_khz.to_string(),
        ];

        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

        if self.verbose {
            tracing::info!("deploy: {}", args.join(" "));
        }

        tracing::info!(
            "flashing {} to {} via lpc21isp (baud={}, xtal_kHz={})",
            firmware_path.display(),
            port,
            self.baud_rate,
            self.xtal_khz,
        );

        let result = run_command(
            &args_ref,
            None,
            None,
            Some(std::time::Duration::from_secs(self.timeout_secs)),
        )
        .await?;

        if result.success() {
            Ok(DeploymentResult {
                success: true,
                message: format!("firmware flashed to {}", port),
                port: Some(port.to_string()),
                stdout: result.stdout,
                stderr: result.stderr,
                outcome: DeployOutcome::FullFlash,
            })
        } else {
            // Return a non-success DeploymentResult instead of Err so the
            // daemon handler can forward lpc21isp's stdout/stderr to the
            // client without losing the diagnostic surface.
            Ok(DeploymentResult {
                success: false,
                message: format!("lpc21isp failed (exit code {})", result.exit_code),
                port: Some(port.to_string()),
                stdout: result.stdout,
                stderr: result.stderr,
                outcome: DeployOutcome::FullFlash,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lpc_deployer_creation() {
        let deployer = LpcDeployer::new("115200", 12_000, 60, None, false);
        assert_eq!(deployer.baud_rate, "115200");
        assert_eq!(deployer.xtal_khz, 12_000);
        assert_eq!(deployer.timeout_secs, 60);
    }

    #[test]
    fn test_lpc_deployer_default_path() {
        let deployer = LpcDeployer::new("115200", 12_000, 60, None, false);
        assert_eq!(deployer.lpc21isp_path, PathBuf::from("lpc21isp"));
    }

    #[test]
    fn test_lpc_deployer_explicit_path() {
        let deployer = LpcDeployer::new(
            "115200",
            12_000,
            60,
            Some(PathBuf::from("/usr/local/bin/lpc21isp")),
            false,
        );
        assert_eq!(
            deployer.lpc21isp_path,
            PathBuf::from("/usr/local/bin/lpc21isp")
        );
    }

    #[test]
    fn with_baud_rate_overrides_default() {
        // Mirrors AvrDeployer::with_baud_rate's TDD test — the deploy CLI's
        // `--baud` flag must reach the deployer and override the board's
        // configured default.
        let deployer = LpcDeployer::new("115200", 12_000, 60, None, false).with_baud_rate("57600");
        assert_eq!(deployer.baud_rate, "57600");
    }

    #[tokio::test]
    async fn test_deploy_requires_port() {
        let deployer = LpcDeployer::new("115200", 12_000, 60, None, false);
        let tmp = tempfile::TempDir::new().unwrap();
        let result = deployer
            .deploy(tmp.path(), "lpc845", Path::new("firmware.hex"), None)
            .await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("serial port required"));
    }

    #[test]
    fn lpc21isp_params_defaults() {
        let params = Lpc21IspParams::default();
        assert_eq!(params.default_baud, "115200");
        assert_eq!(params.xtal_khz, 12_000);
        assert_eq!(params.timeout_secs, 60);
    }

    // ---------- FastLED/fbuild#921 lpc21isp path resolver ----------

    #[test]
    fn find_lpc21isp_env_var_wins_when_pointing_at_real_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fake = tmp.path().join(if cfg!(windows) {
            "lpc21isp.exe"
        } else {
            "lpc21isp"
        });
        std::fs::write(&fake, b"stub").unwrap();

        // SAFETY: single-threaded test process.
        let saved = std::env::var_os(LPC21ISP_PATH_ENV_VAR);
        std::env::set_var(LPC21ISP_PATH_ENV_VAR, &fake);

        let got = find_lpc21isp();

        match saved {
            Some(v) => std::env::set_var(LPC21ISP_PATH_ENV_VAR, v),
            None => std::env::remove_var(LPC21ISP_PATH_ENV_VAR),
        }

        assert_eq!(got.as_deref(), Some(fake.as_path()));
    }

    #[test]
    fn find_lpc21isp_env_var_missing_file_falls_through() {
        // Env var pointing at a non-existent path should be treated as
        // "not configured" rather than a hard error — the resolver
        // continues to the next search location.
        let tmp = tempfile::TempDir::new().unwrap();
        let ghost = tmp.path().join("does-not-exist");

        let saved = std::env::var_os(LPC21ISP_PATH_ENV_VAR);
        std::env::set_var(LPC21ISP_PATH_ENV_VAR, &ghost);

        let got = find_lpc21isp();

        match saved {
            Some(v) => std::env::set_var(LPC21ISP_PATH_ENV_VAR, v),
            None => std::env::remove_var(LPC21ISP_PATH_ENV_VAR),
        }

        // We can't assert the exact fallback (depends on host state), but
        // we CAN assert that the ghost env var did not sneak through.
        assert!(got.as_deref() != Some(ghost.as_path()));
    }

    #[test]
    fn install_hint_mentions_env_var_and_download_source() {
        let hint = lpc21isp_install_hint();
        assert!(
            hint.contains(LPC21ISP_PATH_ENV_VAR),
            "hint must name the env var"
        );
        assert!(
            hint.contains("sourceforge.net"),
            "hint must point at a download"
        );
        assert!(
            hint.contains("C:\\tools\\lpc21isp"),
            "hint must show the Windows convention path"
        );
        assert!(hint.contains("#921"), "hint must cite the tracking issue");
    }

    #[tokio::test]
    async fn deploy_errors_with_install_hint_when_lpc21isp_missing() {
        // Force the deployer to point at an absolute-but-nonexistent
        // path so the file-exists precondition trips.
        let tmp = tempfile::TempDir::new().unwrap();
        let ghost = tmp.path().join("nonexistent-lpc21isp");

        let deployer = LpcDeployer::new("115200", 12_000, 60, Some(ghost), false);

        let fw = tmp.path().join("firmware.hex");
        std::fs::write(&fw, b":00000001FF\n").unwrap();

        let err = deployer
            .deploy(tmp.path(), "lpc845brk", &fw, Some("COM10"))
            .await
            .expect_err("must fail-fast when lpc21isp binary is absent");
        let msg = err.to_string();
        assert!(msg.contains(LPC21ISP_PATH_ENV_VAR), "err={msg}");
        assert!(msg.contains("#921"), "err={msg}");
    }
}
