//! CLI argument types and dispatch.
//!
//! This module hosts the Clap-derived `Cli` / `Commands` / subaction enums,
//! the small helpers that wire argv into them, and the `async_main`
//! dispatcher that fans each parsed subcommand out to a handler in one of
//! the topic submodules.
//!
//! The split exists purely to keep every `.rs` file in this crate under the
//! 900 LOC ceiling enforced by the LOC gate — public behavior is preserved
//! byte-for-byte. Items remain reachable at `crate::cli::<submod>::<Item>`.

pub mod args;
pub mod build;
pub mod clang_tools;
pub mod compile_many;
pub mod daemon_cmd;
pub mod deploy;
pub mod device;
pub mod dispatch;
pub mod lnk;
pub mod monitor_parse;
pub mod pio;
pub mod purge;
pub mod reset;
pub mod show;
pub mod bloat_cmd;

#[cfg(test)]
mod tests;

// Single re-export so `main.rs` can write `cli::async_main()` without
// pulling in the dispatcher's submodule path. All other items stay
// addressable via `crate::cli::<submod>::<item>`.
pub use dispatch::async_main;
