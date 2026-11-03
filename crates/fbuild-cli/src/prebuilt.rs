//! Resolve caller-only experiment flags once and forward them per request.
use std::sync::OnceLock;
static REQUESTED: OnceLock<bool> = OnceLock::new();

pub fn capture_pio_env(cli_on: bool, cli_off: bool) -> std::collections::BTreeMap<String, String> {
    let no = std::env::var("NO_FBUILD_PREBUILT").ok();
    let experiment = std::env::var("FBUILD_PREBUILT").ok();
    let enabled =
        fbuild_packages::prebuilt::enabled(cli_on, cli_off, no.as_deref(), experiment.as_deref());
    let _ = REQUESTED.set(enabled);
    crate::daemon_client::capture_pio_env()
}

pub fn requested() -> bool {
    *REQUESTED.get().unwrap_or(&false)
}
