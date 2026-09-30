//! One integration-test binary for this crate (zackees/ci.yml RUST-005):
//! each top-level `tests/*.rs` file links the crate's whole dependency
//! graph separately, so these modules share one link instead.

mod cache_survives_tar_extract;
mod clangd_check_parity;
mod compile_many_stage2_perf;
mod compile_many_two_stage;
mod esp32_build;
mod esp32s3_size_parity;
mod flag_escaping_lint;
mod lite_scons_acceptance;
mod nxplpc_build_flags;
mod nxplpc_core_compile_commands;
mod stm32_acceptance;
mod teensy30_acceptance;
mod teensy41_acceptance;
mod teensy_build;
mod teensylc_acceptance;
mod zccache_embedded_smoke;
