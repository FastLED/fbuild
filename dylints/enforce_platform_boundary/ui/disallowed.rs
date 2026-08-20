#![allow(dead_code, unused_imports)]

mod libc {}
mod platform_windows {
    pub struct Backend;
}

use libc as native_libc;

#[cfg(windows)]
fn private_windows_only() {}

#[cfg(not(windows))]
use std::os::unix::ffi::OsStrExt as _;

fn main() {
    let _host = cfg!(target_os = "linux");
    let _same_host_again = cfg!(target_os = "linux");
    let _arch = option_env!("CARGO_CFG_TARGET_ARCH");
    let _backend = platform_windows::Backend;
    let _nested_host = format!("{}", cfg!(target_os = "linux"));
    let _nested_os = format!("{}", std::env::consts::OS);
    let _current_image = std::env::current_exe();
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_only_host_selection_is_still_forbidden() {
        assert!(cfg!(unix));
    }
}
