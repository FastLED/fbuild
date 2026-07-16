//! Shared build pipeline helpers used by all platform orchestrators.
//!
//! Extracts the duplicated config-parse → board-load → build-dir-setup → compile → link
//! sequence that was copy-pasted across AVR, Teensy, and ESP32 orchestrators.
//!
//! This module is split into submodules for maintainability; the public API
//! is preserved by re-exporting every previously top-level item here so that
//! `crate::pipeline::Foo` continues to resolve unchanged.

mod build_unflags;
mod compile;
mod context;
mod library;
mod link;
mod project_discovery;
mod sequential;

pub use build_unflags::remove_unflagged_tokens;
pub use compile::{
    compile_local_libraries, compile_sources, generate_compile_db, log_toolchain_version,
};
pub use context::BuildContext;
pub use library::{
    LibraryBuildEnv, add_extra_library_include_dirs, compile_extra_libraries,
    compile_project_as_library, discover_extra_library_roots, pick_archiver,
};
pub use link::{assemble_build_result, handle_link_result};
pub use project_discovery::{discover_project_includes, is_platform_project, is_project_a_library};
pub use sequential::run_sequential_build_with_libs;
