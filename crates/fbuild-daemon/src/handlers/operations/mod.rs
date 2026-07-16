//! Build, deploy, and monitor operation handlers.
//!
//! This module is split into per-RPC submodules so each `.rs` file
//! stays under the 1000-LOC CI gate. The public API is unchanged:
//! callers still reach `build`, `deploy`, `monitor`, `reset`, and
//! `install_deps` through `crate::handlers::operations::*`.

mod build;
mod common;
mod deploy;
mod deploy_port;
mod install_deps;
mod monitor;
mod reset;

#[cfg(test)]
mod tests;

// Public HTTP handlers — these are wired up by `main.rs` and must
// keep their original paths (`crate::handlers::operations::<name>`).
pub use build::build;
pub use deploy::deploy;
pub use install_deps::install_deps;
pub use monitor::monitor;
pub use reset::reset;

// `pub(crate)` re-exports for sibling handler modules
// (`handlers::emulator` consumes these).
pub(crate) use common::{OperationGuard, qemu_extra_build_flags};
pub(crate) use monitor::{MonitorOutcome, MonitorState};
