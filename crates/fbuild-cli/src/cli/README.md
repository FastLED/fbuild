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
- **`daemon_stop.rs`** -- `fbuild daemon stop`: `StopPlan` (the decision), termination + exit confirmation (FastLED/fbuild#1360). Tests in `daemon_stop_tests.rs`
- **`device.rs`** -- `run_device` (list / status / lease / release / take)
- **`doctor.rs`** -- `run_doctor`: read-only host diagnostics; `--check-disk` reports per-root free space and reclaimable caches without building (#1750). Tests in `doctor_tests.rs`
- **`show.rs`** -- `run_show`, `show_daemon_logs`
- **`reset.rs`** -- `run_reset`
- **`lnk.rs`** -- `run_lnk` (pull / check / add)
- **`install.rs`** -- `run_install`: in-process `fbuild install` over `fbuild_build::provision_env`, text / JSON rendering, `--check` exit codes (FastLED/fbuild#1433)
- **`windows_long_paths.rs`** -- Windows preflight, explicit UAC repair, fresh command restart, and graceful daemon handoff (#1673)
- **`buildfs.rs`** -- `run` / `plan_image`: assemble a LittleFS/SPIFFS image from `data/`
  at the partition CSV's filesystem region (#1358). Tests in `buildfs_tests.rs`
- **`buildfs_geometry.rs`** -- `FsGeometry`: the block/page size one volume is
  formatted and mounted with (ESP32 4096/256, ESP8266 from `_FS_block`/`_FS_page`)
- **`buildfs_esp8266.rs`** -- ESP8266 region from the `eesz` menu's `eagle.flash.*.ld`
  (`_FS_start`/`_FS_end`/`_FS_page`/`_FS_block`). Tests in `buildfs_esp8266_tests.rs`
- **`uploadfs.rs`** -- `run`: flash that image to the same offset through the `esptool`
  deployer, with `--verify` readback and a daemon port preempt (#1358). Tests in
  `uploadfs_tests.rs`
- **`tests.rs`** -- unit tests for argument normalization and `fbuild ci` parsing
