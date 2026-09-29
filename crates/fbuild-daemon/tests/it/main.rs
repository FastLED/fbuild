//! One integration-test binary for this crate (zackees/ci.yml RUST-005):
//! each top-level `tests/*.rs` file links the crate's whole dependency
//! graph separately, so these modules share one link instead.

mod build_streaming;
mod legacy_daemon_transition;
mod port_recovery;
mod process_containment;
mod profiling;
mod test_boards_route;
mod test_build_progress_route;
mod test_emu_endpoint;
mod test_libraries_route;
mod test_plotter_route;
