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

/// The one canonical location fbuild manages lpc21isp at. FastLED/fbuild#921
/// treats lpc21isp as a fbuild-owned dependency — auto-install will drop
/// the binary here in a follow-up PR, and the deployer reads it back from
/// the same path. No PATH walk, no out-of-tree fallbacks: if it isn't
/// here (and no env override is set), fbuild owes the user a "how to
/// install" hint, not a silent hunt through system directories.
///
/// Honors `FBUILD_DEV_MODE=1` → `~/.fbuild/dev/tools/…` to match the
/// isolation the rest of `fbuild-paths` applies.
pub fn managed_lpc21isp_path() -> Option<PathBuf> {
    let exe = if cfg!(windows) {
        "lpc21isp.exe"
    } else {
        "lpc21isp"
    };
    let home = home_dir()?;
    let mode = if std::env::var_os("FBUILD_DEV_MODE").is_some() {
        "dev"
    } else {
        "prod"
    };
    Some(home.join(".fbuild").join(mode).join("tools").join(exe))
}

/// Resolve where `lpc21isp` lives on this system.
///
/// Search order (first hit wins):
///
/// 1. `FBUILD_LPC21ISP_PATH` env var — direct override, for CI or dev
///    boxes that want to point at a bespoke build.
/// 2. `~/.fbuild/{prod|dev}/tools/lpc21isp[.exe]` — the canonical
///    fbuild-managed location. Auto-install populates this in a
///    follow-up PR under FastLED/fbuild#921.
///
/// **Deliberately NOT searched:** `PATH`, `C:\tools\lpc21isp\`, Homebrew,
/// apt, etc. Per #921, lpc21isp is fbuild-owned — it lives where fbuild
/// installs it, or it lives at the env-var override, and nowhere else.
/// A silent PATH walk would hide "we forgot to install this" behind a
/// wildcard hit against whatever `lpc21isp` shipped with the host OS,
/// which is the failure mode the deploy pipeline needs to surface, not
/// paper over.
///
/// Returns `None` if no candidate is present — the deployer converts
/// that into a clear "how to install lpc21isp" diagnostic instead of
/// blowing up mid-flash.
pub fn find_lpc21isp() -> Option<PathBuf> {
    if let Some(env_hit) = std::env::var_os(LPC21ISP_PATH_ENV_VAR) {
        let p = PathBuf::from(env_hit);
        if p.is_file() {
            return Some(p);
        }
    }

    if let Some(managed) = managed_lpc21isp_path() {
        if managed.is_file() {
            return Some(managed);
        }
    }

    None
}

/// Build the "install lpc21isp" hint that surfaces on the failing
/// deploy path. Kept as a standalone function so the test module can
/// assert the exact URLs / paths without shelling out.
pub(crate) fn lpc21isp_install_hint() -> String {
    let (tools_dir, exe) = if cfg!(windows) {
        ("~/.fbuild/prod/tools/", "lpc21isp.exe")
    } else {
        ("~/.fbuild/prod/tools/", "lpc21isp")
    };
    format!(
        "lpc21isp not found on PATH or in any fbuild-managed tools dir.\n\
         \n\
         Auto-fetch is not wired yet (tracked under FastLED/fbuild#921).\n\
         Until it lands, install lpc21isp yourself into the location\n\
         fbuild owns, then retry:\n\
         \n\
           1. Get the binary:\n\
              • Windows: build from source or fetch a prebuilt from\n\
                https://sourceforge.net/projects/lpc21isp/files/ .\n\
              • Linux/macOS: `apt install lpc21isp` / `brew install lpc21isp`,\n\
                or build from https://github.com/capiman/lpc21isp source.\n\
           2. Drop it at {tools_dir}{exe} .\n\
           3. Or set {env}=<full path to lpc21isp binary> to point\n\
              anywhere else.\n\
         \n\
         Verify with: `{exe}` (should print usage).",
        env = LPC21ISP_PATH_ENV_VAR
    )
}

/// Anything below the lowest legitimate lpc21isp baud is refused as a
/// serial rate. Board JSONs that reuse `upload.speed` as an openocd /
/// CMSIS-DAP adapter throughput knob (`lpc845brk.json` sets it to
/// `1000`, i.e. 1 MHz SWD clock) fall well under this floor; the
/// deployer treats those as "not really a baud" and falls back to the
/// family default instead of passing kHz through to lpc21isp's autobaud.
///
/// Common lpc21isp baud rates on LPC8xx are 9600 → 230400, with 115200
/// as the reference; picking 4800 as the floor keeps the deployer open
/// to legacy 9600-only harnesses (2× headroom) while catching the
/// CMSIS-DAP kHz values which are all ≤ 4000. FastLED/fbuild#927.
pub(crate) const MIN_LPC21ISP_BAUD: u32 = 4800;

/// FastLED/fbuild#927: resolve the baud that gets passed to lpc21isp.
///
/// - `None` → family default (`params.default_baud`).
/// - `Some(s)` where `s` parses as a `u32 >= MIN_LPC21ISP_BAUD` → use it.
/// - `Some(s)` where `s` parses as a `u32 < MIN_LPC21ISP_BAUD` → refuse
///   and fall back to the family default. Emit a tracing warning so the
///   misconfiguration surfaces on the next deploy run.
/// - `Some(s)` that does not parse as a positive integer → pass through
///   verbatim (lpc21isp's argument parser will complain). Kept lenient
///   so this safety net does not turn into a validation layer.
pub(crate) fn resolve_lpc21isp_baud(
    board_upload_speed: Option<&str>,
    family_default: &str,
) -> String {
    let Some(raw) = board_upload_speed else {
        return family_default.to_string();
    };
    match raw.parse::<u32>() {
        Ok(n) if n >= MIN_LPC21ISP_BAUD => raw.to_string(),
        Ok(n) => {
            tracing::warn!(
                "board `upload.speed = {n}` is below the lpc21isp minimum baud \
                 ({MIN_LPC21ISP_BAUD}); reverting to family default `{family_default}` \
                 (this typically means the board JSON reused `upload.speed` for a \
                 CMSIS-DAP / openocd throughput knob in kHz — see FastLED/fbuild#927)"
            );
            family_default.to_string()
        }
        Err(_) => raw.to_string(),
    }
}

/// FastLED/fbuild#927 (Windows COM10+ path): lpc21isp v1.97 opens the
/// serial port via a bare `CreateFile(<name>, ...)` on Windows. Windows'
/// CreateFile rejects a bare `"COM10"` (or any double-digit COM name)
/// with `ERROR_FILE_NOT_FOUND (2)` — the DOS-device namespace requires
/// the `\\.\` prefix for those, e.g. `\\.\COM10`. Ports COM1–COM9 open
/// unprefixed.
///
/// This helper returns the port string that lpc21isp actually sees on
/// its argv, with the prefix applied when needed. On non-Windows hosts
/// (or ports that already carry the prefix, or non-`COM*` names such as
/// Linux `/dev/ttyUSB0`) the input is returned unchanged.
pub(crate) fn normalize_lpc21isp_port(port: &str) -> String {
    if !cfg!(windows) {
        return port.to_string();
    }
    // Already prefixed — nothing to do.
    if port.starts_with(r"\\.\") {
        return port.to_string();
    }
    // Match `COM<digits>`, case-insensitive on the `COM` prefix. Only
    // apply the prefix to COM10 and above — COM1–COM9 open unprefixed.
    let (head, tail) = port.split_at(port.len().min(3));
    if head.eq_ignore_ascii_case("COM") && tail.bytes().all(|b| b.is_ascii_digit()) {
        if let Ok(n) = tail.parse::<u32>() {
            if n >= 10 {
                return format!(r"\\.\{port}");
            }
        }
    }
    port.to_string()
}

/// FastLED/fbuild#927: return true when the firmware path has the
/// Intel-HEX extension. Case-insensitive so `.HEX` from Windows-authored
/// tooling still hits. Anything else is treated as raw binary and gets
/// spawned WITHOUT the `-hex` flag so lpc21isp does not try to parse
/// binary bytes as Intel-HEX ASCII and abort with exit 1.
/// Given the firmware path the caller handed us (which may be a
/// `.bin` for lpc21isp or already-an-`.elf`), return the sibling
/// `firmware.elf` when one exists on disk. Used by the probe-rs SWD
/// path — probe-rs consumes ELF directly rather than the raw binary.
///
/// Returns `None` when no ELF is present, in which case the SWD path
/// silently falls back to lpc21isp UART ISP.
pub(crate) fn elf_sibling_of(firmware_path: &Path) -> Option<PathBuf> {
    // If the caller already handed us an ELF, use it.
    if firmware_path.extension().and_then(|e| e.to_str()) == Some("elf") && firmware_path.is_file()
    {
        return Some(firmware_path.to_path_buf());
    }
    // Otherwise look for the co-located ELF that the LPC build
    // orchestrator (`fbuild-build` LPC target) always emits alongside
    // the .bin.
    let parent = firmware_path.parent()?;
    let stem = firmware_path.file_stem().and_then(|s| s.to_str())?;
    let candidate = parent.join(format!("{stem}.elf"));
    if candidate.is_file() {
        Some(candidate)
    } else {
        None
    }
}

pub(crate) fn firmware_is_intel_hex(firmware_path: &Path) -> bool {
    firmware_path
        .extension()
        .and_then(|s| s.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("hex"))
        .unwrap_or(false)
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
///
/// FastLED/fbuild#935 / #936: when the board has a probe-rs chip
/// mapping AND a compiled probe-rs binary is on disk AND a CMSIS-DAP
/// probe is attached, this deployer dispatches SWD flash via
/// probe-rs FIRST and only falls back to lpc21isp when that path is
/// unavailable or errors out. The probe-rs route is touchless (no
/// SW3+SW4 button dance) and completes in ~2 seconds against the
/// LPC845-BRK's stock CMSIS-DAP v1.0.7 firmware.
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
    /// probe-rs `--chip` name for this board, or `None` when the
    /// board's MCU family isn't in [`crate::probe_rs::map_board_to_probe_rs_chip`].
    /// A `None` here disables the SWD path entirely for this
    /// deployer instance.
    probe_rs_chip: Option<String>,
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
            probe_rs_chip: None,
        }
    }

    /// Attach a probe-rs `--chip` name so [`Deployer::deploy`] will
    /// try the SWD path before lpc21isp. Callers set this to whatever
    /// [`crate::probe_rs::map_board_to_probe_rs_chip`] returns for
    /// the current board — a `None` result there means the SWD path
    /// isn't wired for this MCU family and this deployer runs
    /// lpc21isp-only.
    pub fn with_probe_rs_chip(mut self, chip: Option<String>) -> Self {
        self.probe_rs_chip = chip;
        self
    }

    /// Build an LPC deployer from board config + lpc21isp params.
    ///
    /// Resolves `lpc21isp` through [`find_lpc21isp`] so the deployer runs
    /// against a real binary on disk. If the resolver comes back empty,
    /// the deployer is still constructed (with the literal string
    /// `"lpc21isp"` as its path) so unit tests can build one without
    /// hardware — the `deploy()` call surfaces the actionable "install
    /// lpc21isp" hint at flash time. FastLED/fbuild#921.
    ///
    /// FastLED/fbuild#927 safety net: `board.upload_speed` under 4800
    /// is refused as a baud value. `lpc845brk.json` (and any board JSON
    /// that reuses `upload.speed` as its openocd/CMSIS-DAP adapter
    /// throughput knob) sets that field to values in kHz (e.g. `1000`),
    /// which cannot legitimately be a UART baud. Fall back to the
    /// family default (115200) rather than passing nonsense to lpc21isp.
    pub fn from_board_config(
        board: &fbuild_config::BoardConfig,
        params: &Lpc21IspParams,
        verbose: bool,
    ) -> Self {
        let baud = resolve_lpc21isp_baud(board.upload_speed.as_deref(), &params.default_baud);
        Self::new(
            &baud,
            params.xtal_khz,
            params.timeout_secs,
            find_lpc21isp(),
            verbose,
        )
        .with_probe_rs_chip(crate::probe_rs::map_board_to_probe_rs_chip(board).map(str::to_string))
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

        // FastLED/fbuild#935 / #936: try the probe-rs SWD path FIRST
        // when we have a compiled probe-rs binary AND an LPC-Link2
        // CMSIS-DAP probe is attached AND the firmware is an ELF.
        // probe-rs speaks CMSIS-DAP over USB directly, so it doesn't
        // need the target chip in ISP mode → no SW3+SW4 button dance,
        // just a hands-off flash in ~2 seconds. When probe-rs isn't
        // available OR the ELF isn't there OR probe-rs errors out, we
        // fall through to the lpc21isp UART ISP path below.
        let probe_rs_candidate = elf_sibling_of(firmware_path)
            .and_then(|elf_path| crate::probe_rs::find_probe_rs().map(|binary| (elf_path, binary)))
            .filter(|_| crate::probe_rs::lpc_link2_probe_attached());
        if let Some((elf_path, probe_rs_binary)) = probe_rs_candidate {
            if let Some(chip) = self.probe_rs_chip.as_deref() {
                let selector = crate::probe_rs::lpc_link2_probe_selector();
                let probe_rs_binary_dbg = probe_rs_binary.clone();
                let elf_path_dbg = elf_path.clone();
                let chip_owned = chip.to_string();
                tracing::info!(
                    "attempting SWD flash via probe-rs (binary={}, chip={}, probe={}, firmware={})",
                    probe_rs_binary_dbg.display(),
                    chip_owned,
                    selector.as_deref().unwrap_or("(first attached)"),
                    elf_path_dbg.display(),
                );
                let selector_owned = selector.clone();
                let selector_ref = selector.clone();
                let elf_for_task = elf_path.clone();
                let probe_rs_for_task = probe_rs_binary.clone();
                let dl = tokio::task::spawn_blocking(move || {
                    crate::probe_rs::run_probe_rs_download(
                        &probe_rs_for_task,
                        &chip_owned,
                        selector_owned.as_deref(),
                        &elf_for_task,
                    )
                })
                .await
                .map_err(|e| {
                    fbuild_core::FbuildError::DeployFailed(format!(
                        "probe-rs spawn_blocking join error: {e}"
                    ))
                })??;

                if dl.success() {
                    // Follow the flash with a reset so the target
                    // starts executing what we just wrote, matching
                    // the lpc21isp `-control` post-flash behaviour.
                    let probe_rs_reset = probe_rs_binary.clone();
                    let chip_for_reset = chip.to_string();
                    let reset = tokio::task::spawn_blocking(move || {
                        crate::probe_rs::run_probe_rs_reset(
                            &probe_rs_reset,
                            &chip_for_reset,
                            selector_ref.as_deref(),
                        )
                    })
                    .await
                    .map_err(|e| {
                        fbuild_core::FbuildError::DeployFailed(format!(
                            "probe-rs reset spawn_blocking join error: {e}"
                        ))
                    })??;

                    let mut combined_stdout = dl.stdout;
                    combined_stdout.push_str(&reset.stdout);
                    let mut combined_stderr = dl.stderr;
                    combined_stderr.push_str(&reset.stderr);

                    return Ok(DeploymentResult {
                        success: true,
                        message: format!(
                            "firmware flashed via probe-rs SWD (chip={chip}, probe={})",
                            selector.as_deref().unwrap_or("first-attached")
                        ),
                        port: Some(port.to_string()),
                        stdout: combined_stdout,
                        stderr: combined_stderr,
                        outcome: DeployOutcome::FullFlash,
                    });
                }

                tracing::warn!(
                    "probe-rs SWD flash exited {}, falling back to lpc21isp UART ISP",
                    dl.exit_code
                );
            } else {
                tracing::debug!("no probe-rs chip mapping for this board; using lpc21isp path");
            }
        }

        // FastLED/fbuild#921: warn if the on-board LPC-Link2 debugger is
        // still on the factory CMSIS-DAP v1.0.7 firmware. That firmware
        // eats DTR/RTS, so `-control` cannot auto-enter ISP and every
        // deploy will need a SW3+SW4 press. The yellow warning names
        // the `fbuild deploy … --upgrade-debugger` command that fixes
        // it once and for all.
        emit_lpc_link2_v1_warning_if_detected(port);

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
        //   lpc21isp [-control] [-wipe] (-hex|-bin) <firmware> <port> <baud> <xtal_kHz>
        //
        // -control : drive DTR/RTS to enter ISP mode and exit it after
        //            flashing. Required for auto-reset boards like the
        //            LPC845-BRK; harmless on boards wired without it
        //            (lpc21isp just no-ops the control lines).
        // -wipe    : full-chip erase before writing. Without this, lpc21isp
        //            only erases the sectors it's about to write, which
        //            leaves stale data in sectors the new firmware happened
        //            to skip. Cheap on 64 KB / 32 KB parts (~250 ms).
        // -hex/-bin: file format selector. `-hex` is lpc21isp's DEFAULT,
        //            so simply omitting it does NOT get raw-binary
        //            behavior — the tool still tries to parse Intel HEX
        //            and errors out with `Missing start of record (':')`
        //            on the first non-`:` byte. Pass `-bin` explicitly
        //            for raw-binary firmware paths so the tool loads
        //            the file byte-for-byte into flash. FastLED/fbuild#927.
        let mut args = vec![
            self.lpc21isp_path.to_string_lossy().to_string(),
            "-control".to_string(),
            "-wipe".to_string(),
        ];
        if firmware_is_intel_hex(firmware_path) {
            args.push("-hex".to_string());
        } else {
            args.push("-bin".to_string());
        }
        // FastLED/fbuild#927 (Windows COM10+ path): lpc21isp v1.97 opens
        // the serial port via a bare `CreateFile("COM10", ...)` on
        // Windows. Windows rejects that with ERROR_FILE_NOT_FOUND (2)
        // for any COMx with x >= 10 — it needs the DOS-device prefix
        // `\\.\COM10`. Apply that here for double-digit ports so the
        // port-open actually succeeds and lpc21isp reaches its
        // synchronize handshake. Ports COM1–COM9 stay unchanged.
        let normalized_port = normalize_lpc21isp_port(port);
        args.extend([
            firmware_path.to_string_lossy().to_string(),
            normalized_port,
            self.baud_rate.clone(),
            self.xtal_khz.to_string(),
        ]);

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

/// Enumerate serial ports, find the one that matches `port_name`, and
/// if its USB descriptor advertises the factory LPC-Link2 CMSIS-DAP
/// v1.0.7 firmware, emit the yellow "please upgrade your debugger"
/// warning to stderr. FastLED/fbuild#921.
///
/// Non-fatal: any errors from the `serialport` API are swallowed since
/// the warning is best-effort — we don't want to fail a deploy just
/// because port enumeration hiccuped.
fn emit_lpc_link2_v1_warning_if_detected(port_name: &str) {
    let Ok(ports) = serialport::available_ports() else {
        return;
    };
    for p in ports {
        if !port_names_match(&p.port_name, port_name) {
            continue;
        }
        if let serialport::SerialPortType::UsbPort(usb) = &p.port_type {
            if crate::lpc_debugger_reflash::looks_like_lpc_link2_v1_firmware(usb.vid, usb.pid) {
                let no_ansi = std::env::var_os("NO_COLOR").is_some()
                    || std::env::var_os("FBUILD_NO_COLOR").is_some();
                let msg = crate::lpc_debugger_reflash::firmware_upgrade_warning_ansi(no_ansi);
                eprintln!("{msg}");
            }
        }
        return;
    }
}

/// Compare a `serialport::SerialPortInfo.port_name` against the
/// user-supplied port string. `serialport` returns names like `COM10`
/// on Windows; users may have typed `\\.\COM10` (the CreateFile prefix
/// FastLED/fbuild#927 applies later in argv) or plain `COM10`. Match
/// on the tail after the DOS-device prefix.
fn port_names_match(sys: &str, user: &str) -> bool {
    let strip = |s: &str| -> String { s.trim_start_matches(r"\\.\").to_ascii_lowercase() };
    strip(sys) == strip(user)
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

    // ---------- FastLED/fbuild#921 firmware-old detection ----------

    #[test]
    fn port_names_match_handles_dos_device_prefix() {
        // Windows: `serialport::available_ports()` returns "COM10",
        // user may have typed "COM10", "com10", or the prefixed
        // `\\.\COM10` form.
        assert!(port_names_match("COM10", "COM10"));
        assert!(port_names_match("COM10", "com10"));
        assert!(port_names_match(r"\\.\COM10", "COM10"));
        assert!(port_names_match("COM10", r"\\.\COM10"));
        assert!(port_names_match(r"\\.\COM10", r"\\.\com10"));
        assert!(!port_names_match("COM10", "COM11"));
        // POSIX paths — pass-through.
        assert!(port_names_match("/dev/ttyUSB0", "/dev/ttyUSB0"));
    }

    #[test]
    fn lpc_link2_v1_firmware_detection_hits_only_expected_vid_pid() {
        use crate::lpc_debugger_reflash::{
            looks_like_lpc_link2_v1_firmware, LPC_LINK2_V1_FIRMWARE_PID, LPC_LINK2_V1_FIRMWARE_VID,
        };
        assert!(looks_like_lpc_link2_v1_firmware(
            LPC_LINK2_V1_FIRMWARE_VID,
            LPC_LINK2_V1_FIRMWARE_PID
        ));
        // Adjacent PIDs / random VIDs — no false hits.
        assert!(!looks_like_lpc_link2_v1_firmware(0x1FC9, 0x0090));
        assert!(!looks_like_lpc_link2_v1_firmware(0x0D28, 0x0204));
        assert!(!looks_like_lpc_link2_v1_firmware(0, 0));
    }

    #[test]
    fn firmware_upgrade_warning_ansi_wraps_yellow_and_names_the_command() {
        use crate::lpc_debugger_reflash::firmware_upgrade_warning_ansi;
        let colored = firmware_upgrade_warning_ansi(false);
        // ANSI SGR yellow-bold opener + reset closer.
        assert!(colored.contains("\x1b[1;33m"));
        assert!(colored.contains("\x1b[0m"));
        assert!(colored.contains("--upgrade-debugger"));
        assert!(colored.contains("#921"));

        // no_ansi=true strips the escapes but keeps the rest.
        let plain = firmware_upgrade_warning_ansi(true);
        assert!(!plain.contains("\x1b["));
        assert!(plain.contains("--upgrade-debugger"));
        assert!(plain.contains("#921"));
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
            hint.contains("~/.fbuild/prod/tools/"),
            "hint must direct the user at the fbuild-managed tools dir \
             (never an out-of-tree C:\\tools\\ path or PATH-walk)"
        );
        assert!(hint.contains("#921"), "hint must cite the tracking issue");
    }

    // ---------- FastLED/fbuild#927 baud + hex-flag fixes ----------

    #[test]
    fn resolve_baud_none_falls_back_to_family_default() {
        assert_eq!(resolve_lpc21isp_baud(None, "115200"), "115200");
    }

    #[test]
    fn resolve_baud_ok_when_at_or_above_floor() {
        assert_eq!(resolve_lpc21isp_baud(Some("115200"), "9600"), "115200");
        assert_eq!(resolve_lpc21isp_baud(Some("57600"), "9600"), "57600");
        // Right AT the floor: still accepted (>= comparison).
        assert_eq!(
            resolve_lpc21isp_baud(Some(&MIN_LPC21ISP_BAUD.to_string()), "9600"),
            MIN_LPC21ISP_BAUD.to_string()
        );
    }

    #[test]
    fn resolve_baud_refuses_cmsis_dap_khz_and_falls_back() {
        // FastLED/fbuild#927: lpc845brk.json ships `upload.speed = 1000`,
        // which is a CMSIS-DAP adapter clock in kHz, not a serial baud.
        // The safety net rejects it and uses the family default instead.
        assert_eq!(resolve_lpc21isp_baud(Some("1000"), "115200"), "115200");
        assert_eq!(resolve_lpc21isp_baud(Some("2000"), "115200"), "115200");
        // Just under the floor: still refused.
        assert_eq!(
            resolve_lpc21isp_baud(Some(&(MIN_LPC21ISP_BAUD - 1).to_string()), "115200"),
            "115200"
        );
    }

    #[test]
    fn resolve_baud_non_numeric_passes_through() {
        // Something exotic like `"auto"` — leave it to lpc21isp to fail
        // clearly rather than silently swap it for the family default.
        assert_eq!(resolve_lpc21isp_baud(Some("auto"), "115200"), "auto");
    }

    #[test]
    #[cfg(windows)]
    fn normalize_port_prefixes_com10_and_above_on_windows() {
        assert_eq!(normalize_lpc21isp_port("COM1"), "COM1");
        assert_eq!(normalize_lpc21isp_port("COM9"), "COM9");
        assert_eq!(normalize_lpc21isp_port("COM10"), r"\\.\COM10");
        assert_eq!(normalize_lpc21isp_port("COM99"), r"\\.\COM99");
        assert_eq!(normalize_lpc21isp_port("com10"), r"\\.\com10");
        // Already prefixed — leave alone.
        assert_eq!(normalize_lpc21isp_port(r"\\.\COM10"), r"\\.\COM10");
        // Non-COM devices — leave alone.
        assert_eq!(normalize_lpc21isp_port("/dev/ttyUSB0"), "/dev/ttyUSB0");
        assert_eq!(normalize_lpc21isp_port("COMx"), "COMx");
    }

    #[test]
    #[cfg(not(windows))]
    fn normalize_port_is_a_noop_on_non_windows() {
        // On POSIX the caller sees /dev/ttyUSBn / /dev/tty.usbserial-*
        // and lpc21isp opens them via plain open(2); no prefix needed.
        assert_eq!(normalize_lpc21isp_port("COM10"), "COM10");
        assert_eq!(normalize_lpc21isp_port("/dev/ttyUSB0"), "/dev/ttyUSB0");
    }

    #[test]
    fn firmware_is_intel_hex_true_for_dot_hex_variants() {
        assert!(firmware_is_intel_hex(Path::new("build/firmware.hex")));
        assert!(firmware_is_intel_hex(Path::new("build/firmware.HEX")));
        assert!(firmware_is_intel_hex(Path::new("firmware.Hex")));
    }

    #[test]
    fn firmware_is_intel_hex_false_for_bin_and_other() {
        // FastLED/fbuild#927: the nxplpc orchestrator emits `.bin`; the
        // deployer must NOT pass `-hex` when that lands.
        assert!(!firmware_is_intel_hex(Path::new("build/firmware.bin")));
        assert!(!firmware_is_intel_hex(Path::new("build/firmware.elf")));
        assert!(!firmware_is_intel_hex(Path::new("build/firmware")));
        assert!(!firmware_is_intel_hex(Path::new("some.hex.zip")));
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
