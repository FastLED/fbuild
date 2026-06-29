//! Thin subprocess wrapper around `teensy_loader_cli` with a bounded retry loop.
//!
//! Why retry: on Windows the loader occasionally bails at byte 1 of the OUT
//! transfer with `error writing to Teensy`. This is a WinUSB/HID driver quirk
//! that PJRC's loader doesn't paper over. Empirically 6/10 cycles in one
//! session hit it; usually 2-3 retries are enough, occasionally a USB reseat
//! is needed (which we can't fix in software, so we surface a clear final
//! diagnostic).
//!
//! Each attempt is a *fresh* subprocess. The loader holds its own libusb
//! handle, and reusing a wedged libusb session would just re-hit the same
//! failure.

use std::path::PathBuf;
use std::time::Duration;

use fbuild_core::subprocess::run_command;
use fbuild_core::Result;

/// Static configuration for one flash attempt.
pub struct FlashConfig {
    /// Resolved path to `teensy_loader_cli` (or the bare name if relying on $PATH).
    pub loader_path: PathBuf,
    /// MCU flag value, e.g. `TEENSY40` / `TEENSY41`.
    pub mcu_name: String,
    /// Wait-for-device flag (typically `-w`).
    pub wait_flag: String,
    /// Verbose flag (typically `-v`).
    pub verbose_flag: String,
    /// Path to the firmware HEX to flash.
    pub firmware_path: PathBuf,
}

/// Outcome of a single subprocess attempt.
#[derive(Debug, Clone)]
pub struct FlashAttempt {
    /// 1-based attempt number.
    pub attempt: u32,
    /// Process exit code (or 137-ish on timeout — see `fbuild_core::subprocess`).
    pub exit_code: i32,
    /// True iff the subprocess reported success.
    pub success: bool,
    /// Captured stdout from `teensy_loader_cli`.
    pub stdout: String,
    /// Captured stderr from `teensy_loader_cli`.
    pub stderr: String,
}

/// Outcome of the full retry loop.
pub struct FlashRunOutcome {
    /// One entry per attempted subprocess (always `>= 1`).
    pub attempts: Vec<FlashAttempt>,
    /// `true` if any attempt succeeded. The last attempt's stdout/stderr is
    /// always the most informative when this is `false`.
    pub success: bool,
}

impl FlashRunOutcome {
    /// stdout from the most recent attempt — what the user wants to see on the
    /// happy path AND on the "all attempts failed" path.
    pub fn last_stdout(&self) -> &str {
        self.attempts
            .last()
            .map(|a| a.stdout.as_str())
            .unwrap_or("")
    }

    /// stderr from the most recent attempt.
    pub fn last_stderr(&self) -> &str {
        self.attempts
            .last()
            .map(|a| a.stderr.as_str())
            .unwrap_or("")
    }

    /// Last attempt's exit code, defaulting to 1 if no attempts ran.
    pub fn last_exit_code(&self) -> i32 {
        self.attempts.last().map(|a| a.exit_code).unwrap_or(1)
    }
}

/// Run `teensy_loader_cli` up to `retries + 1` times, sleeping `backoff_ms`
/// between attempts. Stops on the first success.
///
/// Two-tier timeout: the *first* attempt is given `first_attempt_timeout` to
/// cover the worst-case "user walks up and presses the program button" path
/// after a baud-134 trigger; every subsequent retry uses the smaller
/// `subsequent_attempt_timeout` since by then HalfKay has either already been
/// observed or the device is wedged in a way another retry won't fix.
pub async fn run_with_retry(
    cfg: &FlashConfig,
    retries: u32,
    backoff_ms: u64,
    first_attempt_timeout: Duration,
    subsequent_attempt_timeout: Duration,
    verbose: bool,
) -> Result<FlashRunOutcome> {
    let args: Vec<String> = vec![
        cfg.loader_path.to_string_lossy().to_string(),
        format!("--mcu={}", cfg.mcu_name),
        cfg.wait_flag.clone(),
        cfg.verbose_flag.clone(),
        cfg.firmware_path.to_string_lossy().to_string(),
    ];
    let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

    if verbose {
        tracing::info!("teensy flash: {}", args.join(" "));
    }

    // `retries` is the number of *additional* attempts after the first. The
    // total number of subprocess invocations is therefore `retries + 1`.
    let total_attempts = retries.saturating_add(1);
    let mut attempts: Vec<FlashAttempt> = Vec::with_capacity(total_attempts as usize);

    for attempt in 1..=total_attempts {
        let attempt_timeout = if attempt == 1 {
            first_attempt_timeout
        } else {
            subsequent_attempt_timeout
        };
        let result = run_command(&args_ref, None, None, Some(attempt_timeout)).await?;
        let success = result.success();
        let exit_code = result.exit_code;
        let stdout = result.stdout;
        let stderr = result.stderr;

        if verbose || !success {
            tracing::info!(
                "teensy flash attempt {}/{}: exit {} success={}",
                attempt,
                total_attempts,
                exit_code,
                success
            );
        }

        attempts.push(FlashAttempt {
            attempt,
            exit_code,
            success,
            stdout,
            stderr,
        });

        if success {
            return Ok(FlashRunOutcome {
                attempts,
                success: true,
            });
        }

        // Don't backoff after the last attempt — it just delays the failure
        // surface for no benefit.
        if attempt < total_attempts {
            let last_err = attempts
                .last()
                .map(|a| short_one_line(&a.stderr))
                .unwrap_or_default();
            tracing::warn!(
                "teensy flash attempt {}/{} failed ({}); backing off {} ms",
                attempt,
                total_attempts,
                last_err,
                backoff_ms
            );
            tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
        }
    }

    Ok(FlashRunOutcome {
        attempts,
        success: false,
    })
}

/// Pull a short, single-line excerpt out of multi-line stderr for log lines.
fn short_one_line(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return "<no stderr>".to_string();
    }
    let first_line = trimmed.lines().next().unwrap_or("");
    let max = 120;
    if first_line.len() > max {
        format!("{}…", &first_line[..max])
    } else {
        first_line.to_string()
    }
}

/// True when the user has set `FBUILD_TEENSY_FLASH_RETRIES`.
/// Returns the parsed override or `None` if unset / invalid.
pub fn env_flash_retries_override() -> Option<u32> {
    std::env::var("FBUILD_TEENSY_FLASH_RETRIES")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_one_line_handles_empty() {
        assert_eq!(short_one_line(""), "<no stderr>");
        assert_eq!(short_one_line("   \n\n"), "<no stderr>");
    }

    #[test]
    fn short_one_line_takes_first_line_only() {
        assert_eq!(short_one_line("a\nb\nc"), "a");
        assert_eq!(short_one_line("  hello\nworld"), "hello");
    }

    #[test]
    fn short_one_line_truncates_long_lines() {
        let long = "x".repeat(200);
        let s = short_one_line(&long);
        // 120 ASCII chars + the 3-byte UTF-8 ellipsis (`…`).
        assert!(s.len() <= 120 + '…'.len_utf8());
        assert!(s.ends_with('…'));
    }

    #[test]
    fn flash_run_outcome_defaults_with_no_attempts() {
        let outcome = FlashRunOutcome {
            attempts: Vec::new(),
            success: false,
        };
        assert_eq!(outcome.last_stdout(), "");
        assert_eq!(outcome.last_stderr(), "");
        assert_eq!(outcome.last_exit_code(), 1);
    }

    #[test]
    fn env_override_parses() {
        let _guard = crate::teensy::soft_reboot::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var("FBUILD_TEENSY_FLASH_RETRIES", "9");
        assert_eq!(env_flash_retries_override(), Some(9));
        std::env::set_var("FBUILD_TEENSY_FLASH_RETRIES", "bogus");
        assert_eq!(env_flash_retries_override(), None);
        std::env::remove_var("FBUILD_TEENSY_FLASH_RETRIES");
        assert_eq!(env_flash_retries_override(), None);
    }
}
