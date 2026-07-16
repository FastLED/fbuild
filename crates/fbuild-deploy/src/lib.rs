//! Firmware deployment via platform-specific upload tools.
//!
//! - AVR: avrdude
//! - ESP32: esptool
//! - RP2040: picotool
//! - STM32: st-flash / dfu-util
//! - Teensy: teensy_loader_cli

pub mod avr;
pub mod esp32;
/// Native espflash-backed verify/write path (issue #66).
///
/// Compiled in only when the `espflash-native` cargo feature is enabled.
/// Default builds keep the esptool-subprocess path and pay zero cost in
/// the dep graph (espflash pulls ~30 transitive crates: strum, deku,
/// miette, ...).
#[cfg(feature = "espflash-native")]
pub mod esp32_native;
pub mod lpc;
pub mod lpc_debugger_reflash;
pub mod method_validation;
pub mod probe_rs;
pub mod reset;
pub mod rp2040;
pub mod size_check;
pub mod teensy;

use fbuild_core::Result;
use std::path::Path;

use crate::esp32::FlashRegion;

/// What the deployer actually did on the device.
///
/// Surfaced through `DeploymentResult::outcome` so the daemon's
/// `/api/deploy` response message can distinguish between:
///
/// * a full baseline write (all regions / non-ESP platforms),
/// * a volatile RAM-only load,
/// * a verify-skip (device already held the requested image), and
/// * a selective rewrite (only some ESP32 flash regions were written
///   because bootloader/partitions already matched).
///
/// See GitHub issue #76.
#[derive(Debug, Clone)]
pub enum DeployOutcome {
    /// All regions / the full image were written to the device.
    FullFlash,
    /// A volatile image was accepted into MCU RAM for execution without
    /// programming non-volatile flash. Runtime success is not implied.
    RamLoad,
    /// `esptool verify-flash` matched every region — no write was
    /// performed. The device has been hard-reset by esptool.
    VerifySkip,
    /// Only the listed ESP32 regions were rewritten. Order follows the
    /// caller's intent (usually bootloader → partitions → firmware).
    SelectiveFlash { regions: Vec<FlashRegion> },
}

impl DeployOutcome {
    /// Render a human-readable parenthetical suffix describing the
    /// outcome. Stable — consumers may parse it.
    ///
    /// * `FullFlash`        → `"full flash"`
    /// * `RamLoad`          → `"RAM load accepted"`
    /// * `VerifySkip`       → `"verify skipped, device already matched"`
    /// * `SelectiveFlash`   → `"selective flash: firmware"`, etc.
    pub fn describe(&self) -> String {
        match self {
            DeployOutcome::FullFlash => "full flash".to_string(),
            DeployOutcome::RamLoad => "RAM load accepted".to_string(),
            DeployOutcome::VerifySkip => "verify skipped, device already matched".to_string(),
            DeployOutcome::SelectiveFlash { regions } => {
                let names: Vec<&'static str> = regions
                    .iter()
                    .map(|r| match r {
                        FlashRegion::Bootloader => "bootloader",
                        FlashRegion::Partitions => "partitions",
                        FlashRegion::Firmware => "firmware",
                    })
                    .collect();
                format!("selective flash: {}", names.join(", "))
            }
        }
    }
}

#[derive(Debug)]
pub struct DeploymentResult {
    pub success: bool,
    pub message: String,
    pub port: Option<String>,
    /// Captured stdout from the deploy tool (esptool, avrdude, etc.).
    pub stdout: String,
    /// Captured stderr from the deploy tool.
    pub stderr: String,
    /// What actually happened on the device (full / RAM load / verify-skip /
    /// selective). Surfaced in the daemon's HTTP response message so consumers
    /// can tell an MD5-skip or volatile load from a real flash write.
    pub outcome: DeployOutcome,
}

/// Trait for platform-specific deployers.
///
/// Async per fbuild#813 / #819. The orchestrator calls into platform-specific
/// `deploy` and `post_deploy_recovery` from inside the daemon's tokio runtime;
/// implementations may `.await` subprocess I/O directly. CPU-bound or
/// inherently-sync work (e.g. the native espflash `Flasher` from the
/// `espflash` crate, or the `serialport` crate's blocking open path) must
/// still be wrapped in `tokio::task::spawn_blocking` inside the
/// implementation.
#[async_trait::async_trait]
pub trait Deployer: Send + Sync {
    async fn deploy(
        &self,
        project_dir: &Path,
        env_name: &str,
        firmware_path: &Path,
        port: Option<&str>,
    ) -> Result<DeploymentResult>;

    /// Post-deploy serial-port recovery.
    ///
    /// Called by the daemon's deploy handler after `clear_preemption()`
    /// and before serial monitors are notified to reconnect. The default
    /// impl is a 3-second 100ms fast-poll on the port — most ESP32-S3
    /// boards with native USB re-enumerate in <500ms, so polling returns
    /// far faster than a hard sleep would. Platform-specific deployers
    /// can override this to perform OS-level re-enumeration.
    ///
    /// FastLED/fbuild#605 — this hook is the seam where the LPC + CMSIS-DAP
    /// composite-USB wedge recovery will land. The default fast-poll cannot
    /// clear the Windows error-31 state on a composite USB device whose HID
    /// interface was touched by pyocd; an LPC-specific override calling
    /// `CM_Reenumerate_DevNode_Ex` on the parent hub devnode is the planned
    /// Phase 1 follow-up.
    ///
    // TODO(#605 Phase 1): LPC + CMSIS-DAP wedge-recovery override.
    async fn post_deploy_recovery(&self, port: &str) -> Result<()> {
        let deadline = std::time::Instant::now() + fbuild_core::time::POST_DEPLOY_RECOVERY_DEADLINE;
        let port = port.to_string();
        while std::time::Instant::now() < deadline {
            // serialport::new(...).open() is blocking; offload it. Each
            // probe is short (50ms timeout) so spawn_blocking churn is
            // bounded to ~30 calls over the 3s budget.
            let port_for_probe = port.clone();
            let opened = tokio::task::spawn_blocking(move || {
                serialport::new(&port_for_probe, 115200)
                    .timeout(std::time::Duration::from_millis(50))
                    .open()
                    .is_ok()
            })
            .await
            .unwrap_or(false);
            if opened {
                return Ok(());
            }
            fbuild_core::time::sleep(fbuild_core::time::POLL_100MS).await;
        }
        tracing::warn!("USB re-enumeration: port {} not available after 3s", port);
        Ok(())
    }
}

#[cfg(test)]
mod outcome_tests {
    use super::*;

    #[test]
    fn full_flash_describe() {
        assert_eq!(DeployOutcome::FullFlash.describe(), "full flash");
    }

    #[test]
    fn ram_load_describe() {
        assert_eq!(DeployOutcome::RamLoad.describe(), "RAM load accepted");
    }

    #[test]
    fn verify_skip_describe() {
        assert_eq!(
            DeployOutcome::VerifySkip.describe(),
            "verify skipped, device already matched"
        );
    }

    #[test]
    fn selective_flash_describe_firmware_only() {
        let outcome = DeployOutcome::SelectiveFlash {
            regions: vec![FlashRegion::Firmware],
        };
        assert_eq!(outcome.describe(), "selective flash: firmware");
    }

    #[test]
    fn selective_flash_describe_multiple_regions_ordered_and_lowercase() {
        let outcome = DeployOutcome::SelectiveFlash {
            regions: vec![FlashRegion::Bootloader, FlashRegion::Firmware],
        };
        // Lowercase names joined by ", " — see issue #76 contract.
        assert_eq!(outcome.describe(), "selective flash: bootloader, firmware");
    }

    #[test]
    fn selective_flash_describe_all_three_regions() {
        let outcome = DeployOutcome::SelectiveFlash {
            regions: vec![
                FlashRegion::Bootloader,
                FlashRegion::Partitions,
                FlashRegion::Firmware,
            ],
        };
        assert_eq!(
            outcome.describe(),
            "selective flash: bootloader, partitions, firmware"
        );
    }
}

#[cfg(test)]
mod post_deploy_recovery_tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// Test deployer whose `deploy()` is unimplemented (never invoked
    /// here) but whose `post_deploy_recovery` is observable: it records
    /// the port it saw and that it ran. FastLED/fbuild#605 acceptance
    /// gate — verifies the trait method exists, is dispatched via
    /// `Box<dyn Deployer>`, and that overrides win over the default.
    struct ObservableDeployer {
        called: Arc<AtomicBool>,
        port_seen: Arc<std::sync::Mutex<Option<String>>>,
    }

    #[async_trait::async_trait]
    impl Deployer for ObservableDeployer {
        async fn deploy(
            &self,
            _project_dir: &Path,
            _env_name: &str,
            _firmware_path: &Path,
            _port: Option<&str>,
        ) -> Result<DeploymentResult> {
            unreachable!("deploy not exercised by post_deploy_recovery tests")
        }

        async fn post_deploy_recovery(&self, port: &str) -> Result<()> {
            self.called.store(true, Ordering::SeqCst);
            *self.port_seen.lock().unwrap() = Some(port.to_string());
            Ok(())
        }
    }

    /// A deployer that uses the default `post_deploy_recovery`. Counts
    /// `deploy()` calls so the default-impl test can confirm it didn't
    /// accidentally dispatch through `deploy()`.
    struct DefaultRecoveryDeployer {
        deploy_calls: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl Deployer for DefaultRecoveryDeployer {
        async fn deploy(
            &self,
            _project_dir: &Path,
            _env_name: &str,
            _firmware_path: &Path,
            _port: Option<&str>,
        ) -> Result<DeploymentResult> {
            self.deploy_calls.fetch_add(1, Ordering::SeqCst);
            Ok(DeploymentResult {
                success: true,
                message: "ok".into(),
                port: None,
                stdout: String::new(),
                stderr: String::new(),
                outcome: DeployOutcome::FullFlash,
            })
        }
    }

    #[tokio::test]
    async fn override_is_dispatched_through_box_dyn() {
        let called = Arc::new(AtomicBool::new(false));
        let port_seen = Arc::new(std::sync::Mutex::new(None));
        let dep: Box<dyn Deployer> = Box::new(ObservableDeployer {
            called: Arc::clone(&called),
            port_seen: Arc::clone(&port_seen),
        });

        dep.post_deploy_recovery("COM-fake")
            .await
            .expect("override returns Ok");

        assert!(called.load(Ordering::SeqCst), "override must run");
        assert_eq!(
            port_seen.lock().unwrap().as_deref(),
            Some("COM-fake"),
            "override receives the port verbatim"
        );
    }

    #[tokio::test]
    async fn default_impl_returns_ok_for_nonexistent_port_within_budget() {
        // The default impl polls the port for up to 3 seconds and then
        // returns Ok regardless. Using a port name that cannot possibly
        // exist exercises the slow path. Budget is 4s — the impl itself
        // is bounded at 3s plus scheduling jitter.
        let dep = DefaultRecoveryDeployer {
            deploy_calls: Arc::new(AtomicUsize::new(0)),
        };
        let start = std::time::Instant::now();
        let result = dep
            .post_deploy_recovery("a-port-that-does-not-exist-zzz")
            .await;
        let elapsed = start.elapsed();
        assert!(result.is_ok(), "default impl returns Ok even on timeout");
        assert!(
            elapsed < std::time::Duration::from_secs(4),
            "default impl bounded by ~3s, took {:?}",
            elapsed
        );
        assert_eq!(
            dep.deploy_calls.load(Ordering::SeqCst),
            0,
            "post_deploy_recovery must not invoke deploy()"
        );
    }
}
