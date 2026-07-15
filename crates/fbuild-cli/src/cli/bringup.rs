//! `fbuild bringup` — end-to-end bring-up orchestrator.
//!
//! FastLED/fbuild#697. Consolidates the build → flash → reset →
//! monitor → bring-up RPC pipeline that today is split between
//! fbuild (build + flash halves) and FastLED's Python `bash
//! autoresearch` (port resolution + monitor + RPC validation). The
//! split is exactly what caused the FastLED/FastLED#3300 LPC845-BRK
//! "device looks silent" incident — three DTR/RTS bugs landed in
//! the Python half over weeks because the orchestration spanned
//! two repos.
//!
//! ## Scope of this scaffold
//!
//! This PR ships the CLI shape, the result-reporting tuple, the
//! per-board defaults loader, and the orchestration skeleton that
//! consults [`fbuild_serial`]'s existing primitives
//! ([`fbuild_serial::boards::family_for_vid_pid`],
//! [`BoardFamily::idle_dtr_rts`], [`BoardFamily::handoff_timing`],
//! [`fbuild_serial::esp_reset::dispatch_reset`]). The actual
//! build / flash / monitor steps are STUBBED (return a structured
//! "not wired in scaffold" result); each one gets its own follow-up
//! PR to land the real implementation against an attached board.
//!
//! ## Result shape
//!
//! The 3-way `(remote_ok, log_ok, echo_ok)` tuple is preserved
//! verbatim from FastLED's `autoresearch` because every consumer
//! already knows it. See [`BringupResult`].

use std::time::Instant;

use clap::Args;
use fbuild_core::{FbuildError, Result};
use fbuild_serial::boards::{family_for_vid_pid, vcom_for_env, BoardFamily};

use crate::output;

/// `fbuild bringup` CLI args. Pluggable RPC method / payload /
/// expected-result, with per-board defaults supplied via the env
/// argument's board JSON (`bringup.{method, payload, expected_result}`).
#[derive(Args, Debug)]
pub struct BringupArgs {
    /// Target environment (PlatformIO env name, e.g.
    /// `lpc845brk`, `esp32dev`).
    pub env: String,

    /// JSON-RPC method to send during bring-up. Overrides the
    /// board JSON default.
    #[arg(long)]
    pub rpc_method: Option<String>,

    /// JSON-RPC payload (a JSON array string, e.g. `[4242]`).
    /// Overrides the board JSON default.
    #[arg(long)]
    pub rpc_payload: Option<String>,

    /// Expected RPC result value as a JSON value (e.g. `4242` or
    /// `"ok"`). Overrides the board JSON default.
    #[arg(long)]
    pub expect_result: Option<String>,

    /// Skip the build step (use a pre-built artifact at the env's
    /// configured artifact path). Useful for CI re-runs.
    #[arg(long)]
    pub skip_build: bool,

    /// Don't actually deploy or open the monitor — dry-run to
    /// surface what the orchestrator WOULD do. Useful for CI
    /// without an attached board.
    #[arg(long)]
    pub dry_run: bool,
}

/// Per-board bring-up configuration. Loaded from board JSON
/// (`bringup.*` keys) and overlaid with CLI flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BringupConfig {
    pub rpc_method: String,
    pub rpc_payload: String,
    pub expect_result: String,
}

impl BringupConfig {
    /// Default config used when board JSON has no `bringup.*` block —
    /// matches FastLED's classic `autoresearch` "echo with sentinel
    /// 4242" baseline.
    pub fn default_echo_4242() -> Self {
        Self {
            rpc_method: "echo".to_string(),
            rpc_payload: "[4242]".to_string(),
            expect_result: "4242".to_string(),
        }
    }

    /// Overlay CLI flags on top of board-JSON-loaded defaults.
    pub fn with_overrides(
        mut self,
        method: Option<String>,
        payload: Option<String>,
        expect: Option<String>,
    ) -> Self {
        if let Some(m) = method {
            self.rpc_method = m;
        }
        if let Some(p) = payload {
            self.rpc_payload = p;
        }
        if let Some(e) = expect {
            self.expect_result = e;
        }
        self
    }
}

/// 3-way bring-up result tuple, verbatim from FastLED's
/// `autoresearch` so every consumer already knows the shape.
///
/// - `remote_ok` — flash + reset succeeded; the device is alive
///   on the serial port.
/// - `log_ok` — the device produced the expected boot-banner /
///   ready-line within the [`BoardFamily::handoff_timing`] window.
/// - `echo_ok` — the bring-up RPC returned the expected result.
///
/// **Invariant:** `log_ok` requires `remote_ok`, and `echo_ok`
/// requires `log_ok`. The struct's `Display` implementation walks
/// the tuple in that order so the user sees where the chain broke.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BringupResult {
    pub remote_ok: bool,
    pub log_ok: bool,
    pub echo_ok: bool,
    pub elapsed_ms: u64,
    /// Free-form human-readable detail (e.g. "skipped: dry-run",
    /// "RPC returned 4242, expected 4242", "monitor drained 8s").
    pub details: String,
}

impl BringupResult {
    /// `true` iff all three legs of the tuple are `true`.
    pub fn is_passing(&self) -> bool {
        self.remote_ok && self.log_ok && self.echo_ok
    }
}

impl std::fmt::Display for BringupResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mark = |b: bool| if b { "✓" } else { "✗" };
        writeln!(
            f,
            "bringup: remote={} log={} echo={} ({} ms)",
            mark(self.remote_ok),
            mark(self.log_ok),
            mark(self.echo_ok),
            self.elapsed_ms
        )?;
        if !self.details.is_empty() {
            writeln!(f, "  details: {}", self.details)?;
        }
        Ok(())
    }
}

/// Top-level entry — dispatcher calls this.
pub fn run_bringup(args: BringupArgs) -> Result<()> {
    let result = run_bringup_inner(&args)?;
    // BringupResult's Display impl already terminates with '\n'; strip the
    // trailing newline so result()'s newline doesn't double up.
    output::result(result.to_string().trim_end_matches('\n'));
    if !result.is_passing() && !args.dry_run {
        return Err(FbuildError::DeployFailed(format!(
            "bring-up failed: {result}"
        )));
    }
    Ok(())
}

fn run_bringup_inner(args: &BringupArgs) -> Result<BringupResult> {
    let started = Instant::now();
    let config = BringupConfig::default_echo_4242().with_overrides(
        args.rpc_method.clone(),
        args.rpc_payload.clone(),
        args.expect_result.clone(),
    );

    // Phase 1: resolve VID/PID + board family for the env.
    let vcom = vcom_for_env(&args.env);
    let family = vcom.and_then(|(vid, pid)| family_for_vid_pid(vid, pid));

    if args.dry_run {
        return Ok(dry_run_report(&args.env, vcom, family, &config, started));
    }

    // Phases 2-6 (build, flash, reset, monitor, RPC) are not yet
    // implemented in the scaffold — each is the scope of its own
    // follow-up PR. Until they land, the orchestrator's job is to
    // assemble what it KNOWS and surface a structured "stubbed"
    // result. CI can still call `fbuild bringup ... --dry-run` to
    // validate the orchestration shape end-to-end without hardware.
    Ok(BringupResult {
        remote_ok: false,
        log_ok: false,
        echo_ok: false,
        elapsed_ms: elapsed_ms(started),
        details: format!(
            "scaffold: build/flash/reset/monitor/RPC not yet wired \
             for env `{env}` — see FastLED/fbuild#697 follow-ups. \
             Resolved: family={family:?}, vcom={vcom:?}, \
             config={config:?}. Pass --dry-run to exercise the \
             scaffold shape.",
            env = args.env
        ),
    })
}

fn dry_run_report(
    env: &str,
    vcom: Option<(u16, u16)>,
    family: Option<BoardFamily>,
    config: &BringupConfig,
    started: Instant,
) -> BringupResult {
    let mut details = String::new();
    details.push_str(&format!("env=`{env}`"));
    match vcom {
        Some((vid, pid)) => {
            details.push_str(&format!(" vcom={vid:04X}:{pid:04X}"));
        }
        None => details.push_str(" vcom=(no override; using primary endpoint)"),
    }
    match family {
        Some(f) => {
            let (dtr, rts) = f.idle_dtr_rts();
            let timing = f.handoff_timing();
            let reset = f.reset_method();
            details.push_str(&format!(
                " family={f:?} reset_method={reset:?} \
                 idle_dtr_rts=({dtr},{rts}) \
                 settle={settle}ms drain={drain}ms",
                settle = timing.post_reset_settle_ms,
                drain = timing.boot_drain_ms,
            ));
        }
        None => details.push_str(" family=unknown (no VID:PID mapping)"),
    }
    details.push_str(&format!(
        " rpc={method}({payload})→expect={expect}",
        method = config.rpc_method,
        payload = config.rpc_payload,
        expect = config.expect_result,
    ));
    BringupResult {
        // dry-run reports "would-pass" for the orchestration shape
        // checks that don't need hardware (port lookup, family
        // classification, timing math). The actual remote/log/echo
        // checks require attached hardware — surface that
        // truthfully.
        remote_ok: false,
        log_ok: false,
        echo_ok: false,
        elapsed_ms: elapsed_ms(started),
        details,
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::sync::Once;

    static USB_PROFILE_FIXTURE: Once = Once::new();

    fn install_usb_profile_fixture() {
        USB_PROFILE_FIXTURE.call_once(|| {
            let artifact = serde_json::json!({
                "schema_version": 1,
                "metadata": {},
                "identities": {
                    "16c0:0483": [{
                        "match": {"vid": "16c0", "pid": "0483", "pid_mask": null},
                        "purpose": "runtime",
                        "role": "runtime_cdc",
                        "transport": "usb",
                        "reset": "none",
                        "handoff": "none",
                        "platform": "nxplpc",
                        "family": "lpc11u35-vcom",
                        "generation": null,
                        "interface": "cdc",
                        "provenance": {
                            "source_url": "test://fbuild-cli/bringup",
                            "source_revision": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                            "source_class": "test"
                        },
                        "priority": 100,
                        "allow_ambiguous": false
                    }]
                },
                "boards": {
                    "lpc845brk": {
                        "identities": {
                            "bootloader": [],
                            "compile": [],
                            "probe": [],
                            "runtime": ["16c0:0483"]
                        },
                        "aliases": ["lpc845", "lpc804", "lpcxpresso845max", "lpcxpresso804"]
                    }
                }
            });
            let artifact_bytes = serde_json::to_vec(&artifact).unwrap();
            let digest = Sha256::digest(&artifact_bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            let metadata = serde_json::json!({
                "usb_profiles": "usb-profiles.json",
                "usb_profiles_schema_version": 1,
                "usb_profiles_sha256": digest
            });
            let tmp = tempfile::tempdir().unwrap();
            let meta_path = tmp.path().join("_meta.json");
            let profiles_path = tmp.path().join("usb-profiles.json");
            std::fs::write(&meta_path, serde_json::to_vec(&metadata).unwrap()).unwrap();
            std::fs::write(&profiles_path, artifact_bytes).unwrap();
            fbuild_core::usb::profiles::try_install_verified_cache(&meta_path, &profiles_path)
                .unwrap();
        });
    }

    #[test]
    fn default_config_is_echo_4242() {
        let cfg = BringupConfig::default_echo_4242();
        assert_eq!(cfg.rpc_method, "echo");
        assert_eq!(cfg.rpc_payload, "[4242]");
        assert_eq!(cfg.expect_result, "4242");
    }

    #[test]
    fn config_overrides_replace_only_specified_fields() {
        let cfg = BringupConfig::default_echo_4242().with_overrides(
            Some("ping".to_string()),
            None,
            Some("\"pong\"".to_string()),
        );
        assert_eq!(cfg.rpc_method, "ping");
        assert_eq!(cfg.rpc_payload, "[4242]"); // unchanged
        assert_eq!(cfg.expect_result, "\"pong\"");
    }

    #[test]
    fn result_is_passing_iff_all_three_legs_are_true() {
        let mut r = BringupResult {
            remote_ok: true,
            log_ok: true,
            echo_ok: true,
            elapsed_ms: 100,
            details: String::new(),
        };
        assert!(r.is_passing());
        r.echo_ok = false;
        assert!(!r.is_passing());
        r.echo_ok = true;
        r.log_ok = false;
        assert!(!r.is_passing());
        r.log_ok = true;
        r.remote_ok = false;
        assert!(!r.is_passing());
    }

    #[test]
    fn result_display_walks_the_tuple_left_to_right() {
        let r = BringupResult {
            remote_ok: true,
            log_ok: true,
            echo_ok: false,
            elapsed_ms: 3700,
            details: "RPC returned 4243, expected 4242".to_string(),
        };
        let s = r.to_string();
        // The format consistently spells out remote → log → echo
        // so consumers don't have to guess which leg failed.
        let remote_idx = s.find("remote=").unwrap();
        let log_idx = s.find("log=").unwrap();
        let echo_idx = s.find("echo=").unwrap();
        assert!(remote_idx < log_idx);
        assert!(log_idx < echo_idx);
        assert!(s.contains("3700"));
        assert!(s.contains("4243"));
    }

    /// LPC845-BRK dry-run scaffold: VID:PID resolved via env
    /// (FastLED/fbuild#686), family classified, idle DTR/RTS picked
    /// per #687, handoff timing populated per #691. Details string
    /// surfaces all four.
    #[test]
    fn dry_run_reports_lpc845brk_resolved_state() {
        install_usb_profile_fixture();
        let args = BringupArgs {
            env: "lpc845brk".to_string(),
            rpc_method: None,
            rpc_payload: None,
            expect_result: None,
            skip_build: false,
            dry_run: true,
        };
        let r = run_bringup_inner(&args).unwrap();
        assert!(r.details.contains("lpc845brk"));
        // LPC845-BRK's VCOM is 16C0:0483 (LPC11U35 bridge).
        assert!(r.details.contains("16C0:0483"));
        // Classified as CdcAcmBridge → SWD reset path.
        assert!(r.details.contains("CdcAcmBridge"));
        assert!(r.details.contains("SwdViaCmsisDap"));
        // CDC bridges idle at host-ready (true, true).
        assert!(r.details.contains("idle_dtr_rts=(true,true)"));
        // LPC handoff timing: 500 ms settle + 2000 ms drain (#691).
        assert!(r.details.contains("settle=500ms"));
        assert!(r.details.contains("drain=2000ms"));
        // RPC defaults survived.
        assert!(r.details.contains("echo([4242])"));
        assert!(r.details.contains("expect=4242"));
    }

    /// ESP32 dry-run: env without VCOM override → vcom=None. Family
    /// can't be classified from env alone in that case (we don't
    /// know the VID:PID until the port is enumerated). Details
    /// surface that truthfully.
    #[test]
    fn dry_run_handles_env_without_vcom_override() {
        let args = BringupArgs {
            env: "esp32dev".to_string(),
            rpc_method: None,
            rpc_payload: None,
            expect_result: None,
            skip_build: false,
            dry_run: true,
        };
        let r = run_bringup_inner(&args).unwrap();
        assert!(r.details.contains("vcom=(no override"));
        assert!(r.details.contains("family=unknown"));
    }

    #[test]
    fn non_dry_run_returns_stubbed_result_with_resolved_state() {
        install_usb_profile_fixture();
        let args = BringupArgs {
            env: "lpc845brk".to_string(),
            rpc_method: None,
            rpc_payload: None,
            expect_result: None,
            skip_build: false,
            dry_run: false,
        };
        let r = run_bringup_inner(&args).unwrap();
        // Non-dry-run today reports all-false (scaffold; #697
        // follow-ups land the real build/flash/reset/monitor/RPC).
        assert!(!r.is_passing());
        // But the resolution state IS surfaced for debuggability.
        assert!(r.details.contains("CdcAcmBridge"));
    }

    #[test]
    fn cli_overrides_propagate_into_config() {
        let args = BringupArgs {
            env: "lpc845brk".to_string(),
            rpc_method: Some("status".to_string()),
            rpc_payload: Some("[]".to_string()),
            expect_result: Some("\"ready\"".to_string()),
            skip_build: false,
            dry_run: true,
        };
        let r = run_bringup_inner(&args).unwrap();
        assert!(r.details.contains("rpc=status([])"));
        assert!(r.details.contains("expect=\"ready\""));
    }
}
