//! Selected Linux host mechanics behind [`crate::platform::host`].

use crate::path::NormalizedPath;

/// `$HOME`.
pub(crate) fn home_dir() -> Option<NormalizedPath> {
    std::env::var_os("HOME").map(NormalizedPath::new)
}

/// One-minute load average from `getloadavg(3)`.
pub(crate) fn load_average_1m() -> Option<f64> {
    let mut loads = [0f64; 1];
    // SAFETY: getloadavg writes at most `nelem` (1) doubles into `loads`.
    let n = unsafe { libc::getloadavg(loads.as_mut_ptr(), 1) };
    (n >= 1).then_some(loads[0])
}
