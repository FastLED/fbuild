//! Link-result reporting and final `BuildResult` assembly.

use std::path::{Path, PathBuf};

use fbuild_core::BuildLog;

use crate::linker::LinkResult;
use crate::BuildResult;

/// Log size report and artifacts from a link result.
///
/// When `symbol_analysis_path` is `Some`, the report is written to that path
/// and only a one-liner is logged (unless `verbose` is true, which also streams
/// the full report). When `None`, the report is written to `symbol_analysis.txt`
/// in the build artifacts directory and streamed to the build log.
pub fn handle_link_result(
    link_result: &LinkResult,
    build_log: &mut BuildLog,
    symbol_analysis_path: Option<&Path>,
    verbose: bool,
) {
    if link_result.hex_path.is_some() {
        crate::build_output::log_linking(build_log, "Building firmware.hex");
    } else if link_result.bin_path.is_some() {
        crate::build_output::log_linking(build_log, "Building firmware.bin");
    }

    if let Some(ref size) = link_result.size_info {
        tracing::info!(
            "size: text={} data={} bss={} | flash={}/{} ({:.1}%) ram={}/{} ({:.1}%)",
            size.text,
            size.data,
            size.bss,
            size.total_flash,
            size.max_flash.unwrap_or(0),
            size.flash_percent().unwrap_or(0.0),
            size.total_ram,
            size.max_ram.unwrap_or(0),
            size.ram_percent().unwrap_or(0.0),
        );
        crate::build_output::log_size_report(build_log, size);
    }

    if let Some(ref symbols) = link_result.symbol_map {
        let report = crate::build_output::format_symbol_report(symbols);

        if let Some(path) = symbol_analysis_path {
            // User gave an explicit path — write there, log a one-liner
            if let Err(e) = std::fs::write(path, &report) {
                tracing::warn!("failed to write symbol analysis: {e}");
            } else {
                build_log.push(format!("Symbol analysis written to {}", path.display()));
            }
            // Also stream full report when --verbose
            if verbose {
                crate::build_output::log_symbol_report(build_log, symbols);
            }
        } else {
            // No path — stream to console and write to artifacts dir
            crate::build_output::log_symbol_report(build_log, symbols);
            if let Some(ref elf) = link_result.elf_path {
                if let Some(build_dir) = elf.parent() {
                    let txt_path = build_dir.join("symbol_analysis.txt");
                    if let Err(e) = std::fs::write(&txt_path, &report) {
                        tracing::warn!("failed to write symbol_analysis.txt: {e}");
                    } else {
                        build_log.push(format!("Symbol analysis: {}", txt_path.display()));
                    }
                }
            }
        }
    }

    if let Some(ref elf) = link_result.elf_path {
        crate::build_output::log_artifact(build_log, elf);
    }
    let firmware = link_result
        .hex_path
        .as_ref()
        .or(link_result.bin_path.as_ref());
    if let Some(fw) = firmware {
        crate::build_output::log_artifact(build_log, fw);
    }
}

/// Assemble the final `BuildResult` from link output and build metadata.
pub fn assemble_build_result(
    link_result: LinkResult,
    elapsed: f64,
    platform_label: &str,
    env_name: &str,
    compile_database_path: Option<PathBuf>,
    build_log: BuildLog,
) -> BuildResult {
    tracing::info!("build completed in {:.1}s", elapsed);
    BuildResult {
        success: true,
        firmware_path: link_result.bin_path.or(link_result.hex_path),
        elf_path: link_result.elf_path,
        size_info: link_result.size_info,
        symbol_map: link_result.symbol_map,
        build_time_secs: elapsed,
        message: format!("{} build for {} completed", platform_label, env_name),
        compile_database_path,
        build_log,
    }
}
