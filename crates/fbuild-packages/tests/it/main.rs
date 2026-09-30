//! One integration-test binary for this crate (zackees/ci.yml RUST-005):
//! each top-level `tests/*.rs` file links the crate's whole dependency
//! graph separately, so these modules share one link instead.

mod disk_cache_schema_migration;
mod lnk_e2e;
mod qemu_linux_runtime;
