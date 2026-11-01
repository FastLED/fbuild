//! ESP32 shrink decision + `build_info_<env>.json` emission
//! (FastLED/fbuild#1459).
//!
//! Split out of `build.rs` to keep the orchestrator under the 1000-LOC
//! gate. Two responsibilities live here:
//!
//! 1. [`resolve_shrink`] turns `BuildParams::shrink` into a recorded,
//!    applied decision. The newlib-nano spec file is probed first, so a
//!    toolchain without a nano variant builds unshrunk and says so in both
//!    the build log and the JSON — never a link-time "cannot read spec
//!    file".
//! 2. [`emit_build_info`] writes `build_info_<env>.json` with the
//!    [`ShrinkRecord`] attached. ESP32 had no emitter at all before this,
//!    which is why FastLED's size/symbol tooling fell back to stale
//!    PlatformIO metadata for the one platform shrink actually targets.
//!
//! Both are additive: the metadata schema is the same one the sequential
//! pipeline writes, with `shrink` added as an optional field.

use std::path::Path;

use fbuild_build_engine::shrink::ShrinkMode;
use fbuild_build_engine::shrink::record::ShrinkRecord;
use fbuild_core::BuildLog;
use fbuild_core::Platform;

use super::super::esp32_compiler::Esp32Compiler;
use super::super::esp32_linker::Esp32Linker;
use super::super::printf_thin;
use crate::compiler::Compiler as _;
use crate::linker::Linker as _;

/// Resolve `mode` into the record written to build info, appending the
/// nano link flags to `overlay_link_flags` only when the probe succeeds.
///
/// The caller passes the compiler the link will actually use
/// (`toolchain.get_gcc_path()`), so the probe and the link cannot disagree
/// about the multilib.
pub(crate) async fn resolve_shrink(
    mode: ShrinkMode,
    compiler: &Path,
    caller_path: Option<&str>,
    toolchain_name: &str,
    overlay_link_flags: &mut Vec<String>,
    build_log: &mut BuildLog,
) -> ShrinkRecord {
    if !printf_thin::applies(mode, Platform::Espressif32) {
        // `auto`/`off`/`safe`/`aggressive` still record the request: the
        // JSON then documents that shrink was considered and not applied.
        return printf_thin::record_for(mode, false);
    }
    let available = printf_thin::probe_nano_specs(compiler, caller_path)
        .await
        .unwrap_or(false);
    if available {
        overlay_link_flags.extend(printf_thin::link_flags());
        return printf_thin::record_for(mode, true);
    }
    // Visible, not just filtered `tracing::warn!`: the request is being
    // dropped. The record says the same thing (`resolved: off`, empty
    // `applied`) so terminal output and telemetry agree.
    let warning = format!(
        "WARNING: shrink: --shrink=printf found no nano.specs in {toolchain_name}; \
         building unshrunk"
    );
    build_log.push(warning.clone());
    tracing::warn!("{warning}");
    printf_thin::record_for(mode, false)
}

/// Write `build_info_<env>.json` (+ the `build_info.json` fallback) for a
/// finished ESP32 link, carrying the shrink decision.
///
/// `link_flags` must be the flags the link actually used (including the
/// nano spec file), not the pre-shrink overlay. Write failures degrade to
/// a `tracing::warn!` inside the emitter — metadata never fails a build.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_build_info(
    project_dir: &Path,
    env_name: &str,
    board: &str,
    linker: &Esp32Linker,
    compiler: &Esp32Compiler,
    link_result: &crate::linker::LinkResult,
    link_flags: &[String],
    libs: &[String],
    shrink: &ShrinkRecord,
) {
    // Only platforms that produce a programmable image get metadata.
    let Some(prog) = crate::build_info::pick_prog_path(
        link_result.elf_path.as_deref(),
        link_result.hex_path.as_deref(),
        link_result.bin_path.as_deref(),
    ) else {
        return;
    };
    let info = crate::build_info::BuildInfo::new(
        &prog,
        linker.link_driver_path(),
        linker.link_driver_path(),
        linker.ar_tool_path(),
        linker.objcopy_tool_path(),
        linker.size_tool_path(),
        compiler.c_flags(),
        compiler.cpp_flags(),
        link_flags.to_vec(),
        libs.to_vec(),
        "espressif32".to_string(),
        board.to_string(),
        env_name.to_string(),
    )
    .with_shrink(Some(shrink.clone()));
    if let Err(e) = crate::build_info::emit_build_info(project_dir, env_name, &info) {
        tracing::warn!("emit_build_info failed: {e}");
    }
}

#[cfg(test)]
#[path = "shrink_link_tests.rs"]
mod tests;
