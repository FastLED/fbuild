# CLI

Subcommand handlers and argument types for the `fbuild` binary.

The original `main.rs` exceeded the 900 LOC gate (~3687 lines), so it was
split into this `cli/` module directory. `main.rs` now contains only the
process entry point and the larger-stack trampoline; everything else lives
here and is dispatched from `cli::async_main`.

## Modules

- **`mod.rs`** -- module declarations + re-exports of the public CLI surface
- **`args.rs`** -- Clap-derived `Cli` / `Commands` / `DaemonAction` / `DeviceAction` / `LnkAction`, `KNOWN_SUBCOMMANDS`, `rewrite_args`, `resolve_project_dir`
- **`dispatch.rs`** -- `async_main`: parse argv, set up tracing + Ctrl+C, fan each subcommand out to its handler
- **`monitor_parse.rs`** -- `ParsedMonitorFlags`, `parse_jobs`, `parse_monitor_flags`, `shell_tokenize`
- **`pio.rs`** -- PlatformIO passthrough: `find_pio`, `run_pio_command`, `pio_build`, `pio_deploy`, `pio_monitor`
- **`build.rs`** -- `run_build`, `normalize_path`, `open_in_browser`
- **`deploy.rs`** -- `run_deploy`, `run_test_emu`, `run_monitor`, deploy-route resolution (`CliDeployRoute`, `CliEmulatorKind`)
- **`compile_many.rs`** -- `run_compile_many` (#238) and the `fbuild ci` (#242) adapter helpers
- **`clang_tools.rs`** -- `run_clang_tool`, `run_iwyu`, IWYU cache key + output filtering
- **`purge.rs`** -- `run_purge`, `run_purge_gc`, size formatting, cache listing
- **`daemon_cmd.rs`** -- `run_daemon` and friends (status / restart / kill / locks / cache-stats / gc), process-management helpers
- **`device.rs`** -- `run_device` (list / status / lease / release / take)
- **`show.rs`** -- `run_show`, `show_daemon_logs`
- **`reset.rs`** -- `run_reset`
- **`lnk.rs`** -- `run_lnk` (pull / check / add)
- **`tests.rs`** -- unit tests for argument normalization and `fbuild ci` parsing
