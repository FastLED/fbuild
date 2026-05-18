//! USB-CDC-on-boot warning logic and small public helpers re-exported from `orchestrator`.

use std::path::Path;

use crate::BuildOrchestrator;

/// Create an ESP32 orchestrator (convenience for get_orchestrator dispatch).
pub fn create() -> Box<dyn BuildOrchestrator> {
    Box::new(super::Esp32Orchestrator)
}

/// Determine whether ARDUINO_USB_CDC_ON_BOOT is effectively enabled.
///
/// Combines `board_extra_flags` (a space-separated string from the board JSON) with
/// `user_build_flags` (from platformio.ini `build_flags`).  Board flags are applied
/// first; user flags can override them.  The **last** definition of
/// `-DARDUINO_USB_CDC_ON_BOOT=N` wins, matching C preprocessor semantics.
///
/// Returns `true` only if the final effective value is `1`.
pub fn cdc_on_boot_enabled(board_extra_flags: Option<&str>, user_build_flags: &[String]) -> bool {
    // Collect all flags in application order: board first, then user.
    let board_tokens: Vec<String> = board_extra_flags
        .unwrap_or("")
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();

    let all_flags: Vec<&str> = board_tokens
        .iter()
        .map(|s| s.as_str())
        .chain(user_build_flags.iter().map(|s| s.as_str()))
        .collect();

    let mut effective: Option<bool> = None;

    for flag in &all_flags {
        // Normalise: strip leading whitespace and optional `-D` prefix added by some tools.
        let stripped = flag.trim();
        // Match `-DARDUINO_USB_CDC_ON_BOOT=VALUE` or `ARDUINO_USB_CDC_ON_BOOT=VALUE`
        let without_d = stripped.strip_prefix("-D").unwrap_or(stripped);

        if let Some(value) = without_d.strip_prefix("ARDUINO_USB_CDC_ON_BOOT=") {
            effective = Some(value.trim() == "1");
        }
    }

    effective.unwrap_or(false)
}

/// Emit a `tracing::warn!` if CDC on boot is effectively enabled.
///
/// `ARDUINO_USB_CDC_ON_BOOT=1` initialises the USB CDC port during boot via native
/// USB (ESP32-S3, C3, C6, S2, …).  When no USB host is connected at power-on any
/// call to `Serial.print()` will block indefinitely because the CDC TX buffer has no
/// consumer to drain it.
pub fn warn_if_cdc_on_boot(
    board_name: &str,
    board_extra_flags: Option<&str>,
    user_build_flags: &[String],
) {
    if cdc_on_boot_enabled(board_extra_flags, user_build_flags) {
        tracing::warn!(
            "Board '{}' has ARDUINO_USB_CDC_ON_BOOT=1.  \
             If no USB host is connected at power-on, Serial.print() will block \
             indefinitely.  Add -DARDUINO_USB_CDC_ON_BOOT=0 to build_flags to suppress this warning.",
            board_name
        );
    }
}

/// Check if a project is configured for ESP32 by reading its platformio.ini.
pub fn is_esp32_project(project_dir: &Path, env_name: &str) -> bool {
    crate::pipeline::is_platform_project(project_dir, env_name, fbuild_core::Platform::Espressif32)
}
