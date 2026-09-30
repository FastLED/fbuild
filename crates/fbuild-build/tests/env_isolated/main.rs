//! Integration tests that change process-wide `FBUILD_*` / `ZCCACHE_*`
//! environment variables (FastLED/fbuild#1577 category `env_isolated`).
//!
//! They live apart from `tests/it` so an env change can never leak into an
//! unrelated test, and every test here holds [`ENV_LOCK`] for its whole body
//! so they cannot race each other either.

mod avr_build;
mod dev_daemon_namespace_isolation;
mod eh_frame_strip_esp32;
mod esp32_include_farm_parity;

/// Held by every test in this binary for its whole body.
pub(crate) static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
