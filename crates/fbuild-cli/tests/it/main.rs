//! One integration-test binary for this crate (zackees/ci.yml RUST-005):
//! each top-level `tests/*.rs` file links the crate's whole dependency
//! graph separately, so these modules share one link instead.

mod ci_command;
mod daemon_crash_recovery;
mod lib_select;
mod test_emu_exit_code;
