//! Teensy deployer state machine.
//!
//! See the module-level README and [issue
//! #433](https://github.com/FastLED/fbuild/issues/433) for the failure modes
//! this orchestrates around.
//!
//! Public surface is intentionally identical to the pre-#433 single-file
//! `teensy.rs`: `TeensyDeployer::new`, `TeensyDeployer::from_board_config`,
//! `TeensyLoaderParams`. The state machine lives in `Deployer::deploy`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use fbuild_core::Result;

use crate::{DeployOutcome, Deployer, DeploymentResult};

pub mod first_byte_probe;
pub mod flash;
pub mod halfkay_probe;
pub mod port_discovery;
pub mod soft_reboot;
pub mod usb_type;

/// Loader parameters sourced from MCU config JSON.
///
/// Extended in #433 with the retry / timeout / probe knobs the deployer needs.
/// All new fields have sensible defaults matching the failure-mode budgets
/// argued for in the issue.
pub struct TeensyLoaderParams {
    /// Wait-for-device flag (typically `-w`).
    pub wait_flag: String,
    /// Verbose flag (typically `-v`).
    pub verbose_flag: String,
    /// Hard cap on a *single* flash subprocess once HalfKay has been detected.
    /// Programming a Teensy 4 takes ~3-5 s; the 30 s default leaves slack.
    pub flash_timeout_secs: u64,
    /// How long to wait for HalfKay before programming starts. After a successful
    /// baud-134 trigger this resolves in ~1 s; the larger default is for the
    /// "user walks up and presses the program button" case.
    pub wait_for_halfkay_timeout_secs: u64,
    /// Number of *additional* attempts after the first (so 5 means up to 6
    /// subprocess invocations).
    pub flash_retries: u32,
    /// Inter-attempt backoff for the retry loop.
    pub flash_retry_backoff_ms: u64,
    /// Budget for the post-flash "did any byte arrive?" advisory probe.
    /// `0` disables the probe entirely.
    pub first_byte_timeout_secs: u64,
    /// `true` (default) enables the baud-134 trigger before flashing. Off-switch
    /// for the few hosts where `SerialPortBuilder::baud_rate(134)` is not honored.
    pub baud_134_trigger: bool,
    /// Budget for the post-flash CDC ACM re-enumeration discovery.
    pub post_flash_port_discovery_secs: u64,
}

impl Default for TeensyLoaderParams {
    fn default() -> Self {
        Self {
            wait_flag: "-w".to_string(),
            verbose_flag: "-v".to_string(),
            flash_timeout_secs: 30,
            wait_for_halfkay_timeout_secs: 180,
            flash_retries: 5,
            flash_retry_backoff_ms: 1500,
            first_byte_timeout_secs: 10,
            baud_134_trigger: true,
            post_flash_port_discovery_secs: 5,
        }
    }
}

/// Teensy deployer using `teensy_loader_cli` with the #433 state machine.
pub struct TeensyDeployer {
    loader_path: PathBuf,
    mcu_name: String,
    wait_flag: String,
    verbose_flag: String,
    flash_timeout_secs: u64,
    wait_for_halfkay_timeout_secs: u64,
    flash_retries: u32,
    flash_retry_backoff_ms: u64,
    first_byte_timeout_secs: u64,
    baud_134_trigger: bool,
    post_flash_port_discovery_secs: u64,
    verbose: bool,
}

impl TeensyDeployer {
    pub fn new(
        mcu_name: &str,
        loader_params: &TeensyLoaderParams,
        loader_path: Option<PathBuf>,
        verbose: bool,
    ) -> Self {
        // Honor env overrides at construction time so a single deploy never
        // mixes the user's intent across attempts.
        let flash_retries =
            flash::env_flash_retries_override().unwrap_or(loader_params.flash_retries);
        let first_byte_timeout_secs = first_byte_probe::env_first_byte_timeout_secs_override()
            .unwrap_or(loader_params.first_byte_timeout_secs);
        let baud_134_trigger =
            loader_params.baud_134_trigger && !soft_reboot::baud_134_trigger_disabled();

        Self {
            loader_path: loader_path.unwrap_or_else(|| PathBuf::from("teensy_loader_cli")),
            mcu_name: mcu_name.to_string(),
            wait_flag: loader_params.wait_flag.clone(),
            verbose_flag: loader_params.verbose_flag.clone(),
            flash_timeout_secs: loader_params.flash_timeout_secs,
            wait_for_halfkay_timeout_secs: loader_params.wait_for_halfkay_timeout_secs,
            flash_retries,
            flash_retry_backoff_ms: loader_params.flash_retry_backoff_ms,
            first_byte_timeout_secs,
            baud_134_trigger,
            post_flash_port_discovery_secs: loader_params.post_flash_port_discovery_secs,
            verbose,
        }
    }

    /// Create a Teensy deployer from board config defaults.
    ///
    /// MCU name is the uppercase board ID (e.g. `TEENSY41`).
    pub fn from_board_config(
        board: &fbuild_config::BoardConfig,
        loader_params: &TeensyLoaderParams,
        verbose: bool,
    ) -> Self {
        Self::new(&board.board.to_uppercase(), loader_params, None, verbose)
    }
}

/// Resolve the port the baud-134 trigger should target.
///
/// Preference order:
/// 1. Explicit `--port` from the caller (we trust the user).
/// 2. First currently-enumerated PJRC CDC ACM port.
/// 3. `None` (no trigger possible — fall back to the wait-for-program-button path).
fn resolve_trigger_port(explicit: Option<&str>) -> Option<String> {
    if let Some(p) = explicit {
        if !p.is_empty() {
            return Some(p.to_string());
        }
    }
    port_discovery::first_pjrc_cdc_port()
}

#[async_trait::async_trait]
impl Deployer for TeensyDeployer {
    async fn deploy(
        &self,
        _project_dir: &Path,
        _env_name: &str,
        firmware_path: &Path,
        port: Option<&str>,
    ) -> Result<DeploymentResult> {
        // ---- 1. Pre-flash port snapshot --------------------------------
        let pre_snapshot = port_discovery::snapshot_port_names();
        // Normalize the caller-supplied port once: treat `Some("")` as
        // `None` so we never propagate an empty string into
        // `DeploymentResult.port` (the daemon would forward it verbatim to
        // the monitor and produce a `failed to open ""` error).
        let explicit_port: Option<String> = port
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let trigger_port = resolve_trigger_port(explicit_port.as_deref());

        // Best-effort `usb_type` advisory — log it now so the user sees the
        // hint *before* we discover the firmware is silent.
        let usb_advisory = usb_type::read_usb_type_near(firmware_path)
            .map(|raw| usb_type::UsbTypeAdvisory::classify(&raw))
            .and_then(|advisory| advisory.advisory_message());
        if let Some(msg) = usb_advisory.as_deref() {
            tracing::warn!("teensy deploy advisory: {}", msg);
        }

        // ---- 2. Baud-134 soft reboot (if applicable) -------------------
        if self.baud_134_trigger {
            if let Some(ref tp) = trigger_port {
                if port_discovery::is_pjrc_cdc(tp) {
                    // serialport open + DTR/RTS + sleep is blocking;
                    // offload so the runtime stays responsive.
                    let tp_owned = tp.clone();
                    let verbose = self.verbose;
                    let trigger_result =
                        tokio::task::spawn_blocking(move || -> Result<(bool, String)> {
                            let triggered = soft_reboot::baud_134_trigger(&tp_owned, verbose)?;
                            Ok((triggered, tp_owned))
                        })
                        .await
                        .unwrap_or_else(|e| {
                            Err(fbuild_core::FbuildError::DeployFailed(format!(
                                "baud-134 trigger task panicked: {}",
                                e
                            )))
                        });
                    match trigger_result {
                        Ok((true, port_name)) => {
                            // Confirm the device left CDC class (entered HalfKay).
                            let _ = tokio::task::spawn_blocking(move || {
                                halfkay_probe::wait_for_disappearance(
                                    &port_name,
                                    Duration::from_secs(3),
                                )
                            })
                            .await;
                        }
                        Ok((false, _)) => {
                            // Already gone — treat as already-HalfKay.
                        }
                        Err(e) => {
                            tracing::warn!(
                                "baud-134 trigger failed ({}); falling back to teensy_loader_cli -w",
                                e
                            );
                        }
                    }
                } else if self.verbose {
                    tracing::info!(
                        "teensy: port {} is not a PJRC CDC device; skipping baud-134 trigger",
                        tp
                    );
                }
            }
        }

        // ---- 3. Flash with bounded retry -------------------------------
        // Note: `teensy_loader_cli -w` performs its own wait-for-HalfKay at the
        // HID level. The `wait_for_halfkay_timeout_secs` budget bounds the
        // first attempt; subsequent retries reuse the smaller `flash_timeout_secs`
        // since by then HalfKay should be ready immediately after re-trigger.
        let flash_cfg = flash::FlashConfig {
            loader_path: self.loader_path.clone(),
            mcu_name: self.mcu_name.clone(),
            wait_flag: self.wait_flag.clone(),
            verbose_flag: self.verbose_flag.clone(),
            firmware_path: firmware_path.to_path_buf(),
        };

        if self.verbose {
            tracing::info!(
                "flashing {} via teensy_loader_cli ({})",
                firmware_path.display(),
                self.mcu_name
            );
        }

        let flash_outcome = flash::run_with_retry(
            &flash_cfg,
            self.flash_retries,
            self.flash_retry_backoff_ms,
            // First attempt may need to wait for the user to press the
            // program button on a fresh board — full HalfKay budget.
            Duration::from_secs(self.wait_for_halfkay_timeout_secs),
            // Subsequent retries: HalfKay was either already observed (the
            // baud-134 trigger left a fresh window open) or the device is
            // wedged in a way another retry won't fix — use the tighter
            // per-flash budget so a wedged board can't burn many minutes
            // before falling through to the structured diagnostic.
            Duration::from_secs(self.flash_timeout_secs),
            self.verbose,
        )
        .await?;

        if !flash_outcome.success {
            let attempt_count = flash_outcome.attempts.len();
            return Ok(DeploymentResult {
                success: false,
                message: format!(
                    "teensy_loader_cli failed after {} attempt(s) (exit {}); \
                     check USB cable, hub topology, and try pressing the program button",
                    attempt_count,
                    flash_outcome.last_exit_code()
                ),
                port: explicit_port.clone(),
                stdout: flash_outcome.last_stdout().to_string(),
                stderr: flash_outcome.last_stderr().to_string(),
                outcome: DeployOutcome::FullFlash,
            });
        }

        // ---- 4. Post-flash CDC ACM port discovery ----------------------
        // wait_for_new_cdc_port polls the OS port list with 100ms sleeps;
        // offload to a blocking thread so the runtime is free during the
        // up-to-5s discovery window.
        let discovery_pre = pre_snapshot.clone();
        let discovery_window = Duration::from_secs(self.post_flash_port_discovery_secs);
        let discovery_outcome =
            tokio::task::spawn_blocking(move || {
                port_discovery::wait_for_new_cdc_port(&discovery_pre, discovery_window)
            })
            .await
            .unwrap_or(port_discovery::NewPortOutcome::TimedOut);
        let new_port = match discovery_outcome {
            port_discovery::NewPortOutcome::Found(name) => Some(name),
            port_discovery::NewPortOutcome::TimedOut => {
                // Re-enumeration may have reused the same port name (the
                // common case on Linux/macOS). Fall back to the explicit
                // port the caller asked for, and if none was given, to the
                // PJRC port we triggered against — that's the device the
                // monitor wants to attach to.
                explicit_port.clone().or_else(|| trigger_port.clone())
            }
        };

        // ---- 5. Optional first-byte probe ------------------------------
        let mut message_suffix = String::new();
        if let Some(ref np) = new_port {
            if self.first_byte_timeout_secs > 0 {
                let np_owned = np.clone();
                let timeout = Duration::from_secs(self.first_byte_timeout_secs);
                let outcome = tokio::task::spawn_blocking(move || {
                    first_byte_probe::probe(&np_owned, 115_200, timeout)
                })
                .await
                .unwrap_or(first_byte_probe::FirstByteOutcome::Disabled);
                match outcome {
                    first_byte_probe::FirstByteOutcome::SawByte { elapsed_ms } => {
                        message_suffix.push_str(&format!("; first byte at {} ms", elapsed_ms));
                    }
                    first_byte_probe::FirstByteOutcome::Silent { .. } => {
                        let diag =
                            first_byte_probe::silent_diagnostic(np, self.first_byte_timeout_secs);
                        tracing::warn!("teensy deploy: {}", diag);
                        message_suffix.push_str("; firmware-silent advisory: see stderr");
                    }
                    first_byte_probe::FirstByteOutcome::Disabled => {}
                }
            }
        }

        let attempt_count = flash_outcome.attempts.len();
        let port_str = new_port.clone().unwrap_or_else(|| "<unknown>".to_string());
        let mut final_message = if attempt_count == 1 {
            format!(
                "firmware flashed to {} via {}{}",
                self.mcu_name, port_str, message_suffix
            )
        } else {
            format!(
                "firmware flashed to {} via {} after {} attempt(s){}",
                self.mcu_name, port_str, attempt_count, message_suffix
            )
        };
        if let Some(adv) = usb_advisory {
            final_message.push_str(&format!("; usb_type advisory: {}", adv));
        }

        Ok(DeploymentResult {
            success: true,
            message: final_message,
            port: new_port,
            stdout: flash_outcome.last_stdout().to_string(),
            stderr: flash_outcome.last_stderr().to_string(),
            outcome: DeployOutcome::FullFlash,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_teensy_deployer_creation() {
        let deployer = TeensyDeployer::new("TEENSY41", &TeensyLoaderParams::default(), None, false);
        assert_eq!(deployer.mcu_name, "TEENSY41");
        assert_eq!(deployer.wait_flag, "-w");
        // Defaults from #433 (not the old 60s blanket).
        assert_eq!(deployer.flash_timeout_secs, 30);
        assert_eq!(deployer.wait_for_halfkay_timeout_secs, 180);
    }

    #[test]
    fn test_teensy_deployer_from_board_config() {
        let board = fbuild_test_support::board_for_test("teensy41");
        let deployer =
            TeensyDeployer::from_board_config(&board, &TeensyLoaderParams::default(), false);
        assert_eq!(deployer.mcu_name, "TEENSY41");
    }

    #[test]
    fn test_teensy_deployer_teensy40() {
        let board = fbuild_test_support::board_for_test("teensy40");
        let deployer =
            TeensyDeployer::from_board_config(&board, &TeensyLoaderParams::default(), false);
        assert_eq!(deployer.mcu_name, "TEENSY40");
    }

    #[test]
    fn defaults_match_issue_433() {
        let p = TeensyLoaderParams::default();
        assert_eq!(p.flash_timeout_secs, 30);
        assert_eq!(p.wait_for_halfkay_timeout_secs, 180);
        assert_eq!(p.flash_retries, 5);
        assert_eq!(p.flash_retry_backoff_ms, 1500);
        assert_eq!(p.first_byte_timeout_secs, 10);
        assert!(p.baud_134_trigger);
        assert_eq!(p.post_flash_port_discovery_secs, 5);
    }

    #[test]
    fn resolve_trigger_port_prefers_explicit() {
        assert_eq!(resolve_trigger_port(Some("COM7")), Some("COM7".to_string()));
    }

    #[test]
    fn resolve_trigger_port_ignores_empty_explicit() {
        // An empty string from the CLI is not a real port — fall through to
        // discovery rather than passing "" to serialport::new.
        let resolved = resolve_trigger_port(Some(""));
        // We can't assert what discovery returned (CI may have no PJRC port),
        // but we can assert it's not the empty string.
        assert_ne!(resolved.as_deref(), Some(""));
    }
}
