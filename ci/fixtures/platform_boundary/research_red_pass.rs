#![allow(dead_code, unused_imports)]

// Phase-1 RED evidence: all of these constructs compile before the boundary
// from #1306 exists. Phase 2 converts them into negative Dylint fixtures.
#[cfg(windows)]
fn private_windows_only() {}

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt as _;

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt as _;

pub fn research_red_evidence() {
    let _is_windows = cfg!(windows);
    let _host_os = std::env::consts::OS;
    let _target_os = option_env!("CARGO_CFG_TARGET_OS");
}
